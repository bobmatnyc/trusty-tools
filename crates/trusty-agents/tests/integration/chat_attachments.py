#!/usr/bin/env python3
"""Why: component tests cannot prove image bytes survive the API, provider, and durable history boundaries (#7370).
What: run frozen binaries in isolated roots; assert preparation, dispatch, owned assets, restart, auth, and failure behavior.
Test: the command documented in tests/integration/README.md runs these assertions. No actions without --run.
The default observer stays on loopback. --real-vision permits exactly one OpenRouter request with an explicitly supplied credential-store path.
"""
import argparse, base64, hashlib, json, os, pathlib, secrets, socket, socketserver, subprocess, tempfile, threading, time, traceback, urllib.request, urllib.error, uuid
import http.server as http_server
import shutil, struct, zlib
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--run', action='store_true')
parser.add_argument('--real-vision', action='store_true')
parser.add_argument('--assistant-root-parent', required=True, type=pathlib.Path, help='Existing visible directory for a unique synthetic assistant store')
parser.add_argument('--credential-store', type=pathlib.Path, help='Explicit existing shared store, used only with --real-vision')
for name in ['tagent', 'memory', 'search']:
    parser.add_argument('--' + name)
    parser.add_argument('--' + name + '-sha256')
args = parser.parse_args()
if not args.run:
    parser.error('No runtime started: explicit --run and frozen binary hashes required')
assert args.assistant_root_parent.is_dir(), 'Assistant parent must exist'
assert bool(args.credential_store) == args.real_vision, '--real-vision requires explicit --credential-store; deterministic mode forbids it'
if args.credential_store:
    assert args.credential_store.is_file(), 'Credential store does not exist'
args.assistant_root_parent = args.assistant_root_parent.resolve()

def synthetic_image():
    """Generate an RGB PNG using only the standard library."""

    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    pixels = bytearray()
    for y in range(160):
        pixels.append(0)
        for x in range(320):
            color = (255, 0, 0) if (x - 75) ** 2 + (y - 80) ** 2 <= 2500 else (0, 0, 255) if 195 <= x <= 295 and 30 <= y <= 130 else (255, 255, 255)
            pixels.extend(color)
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 320, 160, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(pixels)) + chunk(b'IEND', b'')
for name in ['tagent', 'memory', 'search']:
    binary = getattr(args, name)
    expected = getattr(args, name + '_sha256')
    assert binary and expected
    binary = str(pathlib.Path(binary).resolve())
    setattr(args, name, binary)
    assert hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest() == expected, (name, 'binary hash mismatch')
root = pathlib.Path(tempfile.mkdtemp(prefix='qa-attachment-integrated-', dir='/tmp')).resolve()
paths = {n: root / n for n in ['home', 'project', 'runtime', 'search-data', 'mpm']}
for p in paths.values():
    p.mkdir(mode=0o700)
paths['assistants'] = pathlib.Path(tempfile.mkdtemp(prefix='qa-attachment-integrated-', dir=args.assistant_root_parent)).resolve()
agents = paths['project'] / '.trusty-agents/agents'
agents.mkdir(parents=True)
for name in ['assistant', 'qa-attachment-secondary']:
    p = agents / name
    p.mkdir()
    (p / 'persona.md').write_text('Inspect only synthetic attachments. Reply briefly. Do not use tools.\n')
    (p / 'agent.toml').write_text("[agent]\nname='" + name + "'\nrole='assistant'\nkind='assistant'\nrunner='subprocess'\nprovider_id='openrouter'\nmodel='openai/gpt-4o-mini'\ndescription='Synthetic attachment QA'\n[llm]\nmax_tokens=64\ntemperature=0\n[tools]\nallow=['memory_remember','memory_write','memory_recall']\n[permissions]\nscopes=['memory.read','memory.write']\n")
secondary = agents / 'qa-attachment-secondary' / 'agent.toml'
secondary.write_text(secondary.read_text().replace("name='qa-attachment-secondary'\n", "name='qa-attachment-secondary'\nextends='assistant'\n"))
records = []
calls = []
children = {}
all_pids = []
logs = []
server = None
compat_server = None
credential_link = None
failure = None
control = {'real_used': False}
api_token = secrets.token_urlsafe(32)
memory_sock = paths['runtime'] / 'trusty-memory/trusty-memory.sock'
search_sock = paths['runtime'] / 'trusty-search/trusty-search.sock'

def record(label, value):
    records.append({'step': label, 'value': value})
    print(json.dumps({'step': label, 'value': value}), flush=True)

def bounded(response, cap):
    length = response.headers.get('Content-Length')
    assert length is None or int(length) <= cap, 'HTTP response too large'
    raw = response.read(cap + 1)
    assert len(raw) <= cap, 'HTTP response exceeded cap'
    return raw

def rpc_raw(path, request):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.settimeout(30)
        s.connect(str(path))
        s.sendall((json.dumps(request) + '\n').encode())
        s.shutdown(socket.SHUT_WR)
        raw = b''
        while b'\n' not in raw:
            data = s.recv(65536)
            if not data:
                break
            raw += data
            assert len(raw) <= 16 * 1024 * 1024, 'UDS response too large'
        return json.loads(raw.split(b'\n')[0])

def memory_tool(name, arguments):
    response = rpc_raw(memory_sock, {'jsonrpc': '2.0', 'id': 1, 'method': 'tools/call', 'params': {'name': name, 'arguments': arguments}})
    assert 'error' not in response, response
    result = response['result']
    assert not result.get('isError'), result
    return json.loads(result['content'][0]['text'])

class Provider(http_server.BaseHTTPRequestHandler):

    def log_message(self, *args):
        pass

    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(b'{"data":[],"models":[]}')

    def do_POST(self):
        size = int(self.headers.get('Content-Length', '0'))
        assert 0 < size <= 15 * 1024 * 1024
        self.connection.settimeout(20)
        req = json.loads(self.rfile.read(size))
        calls.append(req)
        record('provider_payload', {'model': req.get('model'), 'messages': req.get('messages'), 'tools_count': len(req.get('tools', []))})
        if args.real_vision:
            if control['real_used']:
                self.send_response(429)
                self.end_headers()
                self.wfile.write(b'{"error":"QA real inference limit reached"}')
                return
            control['real_used'] = True
            assert self.path.endswith('/chat/completions') and req['model'] == 'openai/gpt-4o-mini'
            assert 0 < req.get('max_tokens', 0) <= 64, 'Live vision output must remain bounded'
            request = urllib.request.Request('https://openrouter.ai/api/v1/chat/completions', data=json.dumps(req).encode(), headers={'Content-Type': 'application/json', 'Authorization': self.headers.get('Authorization', '')})
            try:
                with urllib.request.urlopen(request, timeout=90) as response:
                    status = response.status
                    raw = bounded(response, 1024 * 1024)
            except urllib.error.HTTPError as e:
                status = e.code
                raw = bounded(e, 1024 * 1024)
            record('real_provider_result', {'status': status, 'body': json.loads(raw)})
        else:
            raw = json.dumps({'id': 'qa-attachment-response', 'object': 'chat.completion', 'created': int(time.time()), 'model': req.get('model'), 'choices': [{'index': 0, 'message': {'role': 'assistant', 'content': 'Synthetic attachment received.'}, 'finish_reason': 'stop'}], 'usage': {'prompt_tokens': 12, 'completion_tokens': 5, 'total_tokens': 17}}).encode()
            status = 200
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(raw)

class OldDaemon(socketserver.StreamRequestHandler):

    def handle(self):
        raw = self.rfile.readline(16 * 1024 * 1024)
        req = json.loads(raw)
        method = req.get('params', {}).get('name', req.get('method'))
        record('old_daemon_request', {'method': method})
        if control.get('fail_history_probe') and method == 'chat_session_get':
            result = {'jsonrpc': '2.0', 'id': req['id'], 'error': {'code': -32603, 'message': 'Synthetic memory history unavailable'}}
        elif control.get('fail_assets') and method == 'chat_asset_get':
            result = {'jsonrpc': '2.0', 'id': req['id'], 'error': {'code': -32603, 'message': 'Synthetic memory asset unavailable'}}
        elif method == 'chat_asset_capabilities' and (not control.get('pass_capabilities')):
            result = {'jsonrpc': '2.0', 'id': req['id'], 'result': {'isError': True, 'content': [{'type': 'text', 'text': 'Unknown tool chat_asset_capabilities (synthetic old-daemon capability fixture)'}]}}
        else:
            result = rpc_raw(memory_sock, req)
        self.wfile.write((json.dumps(result) + '\n').encode())

class UDS(socketserver.ThreadingUnixStreamServer):
    daemon_threads = True

def start(name, binary, argv, extra=None):
    log = open(root / (name + '-' + str(len(all_pids)) + '.log'), 'wb')
    logs.append(log)
    env2 = dict(env)
    env2.update(extra or {})
    child = subprocess.Popen([binary] + argv, env=env2, cwd=paths['project'], stdin=subprocess.DEVNULL, stdout=log, stderr=log)
    children[name] = child
    all_pids.append(child.pid)
    record('launch_' + name, {'pid': child.pid, 'binary': binary, 'sha256': hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest(), 'args': argv})

def stop(name):
    child = children.pop(name, None)
    if child:
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=5)
        record('stopped_' + name, {'pid': child.pid, 'returncode': child.returncode})

def wait(label, predicate, seconds=60):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        assert all((c.poll() is None for c in children.values())), 'Owned child exited unexpectedly'
        try:
            result = predicate()
            if result:
                return result
        except (ConnectionRefusedError, FileNotFoundError, urllib.error.URLError, socket.timeout):
            pass
        time.sleep(0.5)
    raise AssertionError(label + ' timed out')

def http(method, path, body=None, status=200, auth='valid', binary=False, origin=None):
    headers = {'Content-Type': 'application/json'}
    if auth == 'valid':
        headers['Authorization'] = 'Bearer ' + api_token
    elif auth == 'wrong':
        headers['Authorization'] = 'Bearer synthetic-invalid-token'
    if origin:
        headers['Origin'] = origin
    req = urllib.request.Request(base + path, data=None if body is None else json.dumps(body).encode(), method=method, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=20) as response:
            code = response.status
            raw = bounded(response, 15 * 1024 * 1024 if path == '/api/chat-attachments/prepare' else 6 * 1024 * 1024)
            response_headers = {k.lower():v for k,v in response.headers.items()}
    except urllib.error.HTTPError as e:
        code = e.code
        raw = bounded(e, 1024 * 1024)
        response_headers = {k.lower():v for k,v in e.headers.items()}
    if binary and code == 200:
        value = {'sha256': hashlib.sha256(raw).hexdigest(), 'bytes': len(raw)}
    else:
        try:
            value = json.loads(raw)
        except json.JSONDecodeError:
            value = {'text': raw.decode(errors='replace')[:2000]}
    record('http', {'method': method, 'path': path, 'status': code, 'response': value if len(json.dumps(value)) < 16000 else {'truncated': json.dumps(value)[:16000]}})
    assert code == status, (path, code, str(value)[:1000])
    return (raw, response_headers) if binary and code == 200 else value

def task(text, attachments=None, provider='openrouter', model='openai/gpt-4o-mini', success=True, expected_status=None):
    submit = http('POST', '/api/task', {'task': text, 'agent': 'assistant', 'provider_id': provider, 'model_id': model, 'attachments': attachments or []}, 202)

    def terminal():
        result = http('GET', '/api/task/' + submit['id'])
        return result if result['status'] not in ['running', 'pending'] else None
    result = wait('task ' + submit['id'], terminal, 120 if args.real_vision else 45)
    if expected_status is not None:
        assert result['status'] == expected_status, result
    else:
        assert (result['status'] == 'success') == success, result
    return result

def assert_payload(index, image_bytes, table=None):
    assert len(calls) > index, 'Provider did not receive request'
    body = calls[index]
    serialized = json.dumps(body)
    url = 'data:image/png;base64,' + base64.b64encode(image_bytes).decode()
    assert url in serialized, 'Provider missed exact inline image bytes'
    if table:
        texts = []
        for message in body['messages']:
            content = message.get('content')
            if isinstance(content, str):
                texts.append(content)
            elif isinstance(content, list):
                texts.extend(part['text'] for part in content if part.get('type') == 'text')
        tables = []
        for text in texts:
            for line in text.splitlines():
                try:
                    tables.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
        expected = {key: table[key] for key in ['name', 'source_format', 'sheets']}
        assert expected in tables, 'Provider missed exact table values'
    record('payload_verified', {'call': index, 'inline_image_bytes_equal': True, 'table_values_checked': table is not None})
try:
    server = http_server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        port = s.getsockname()[1]
    base = 'http://127.0.0.1:' + str(port)
    env = {'PATH': os.pathsep.join(dict.fromkeys([str(pathlib.Path(getattr(args, n)).resolve().parent) for n in ['tagent', 'memory', 'search']] + os.defpath.split(os.pathsep))), 'HOME': str(paths['home']), 'TAGENT_PROJECT_DIR': str(paths['project']), 'TAGENT_CONFIG_DIR': str(agents), 'TAGENT_ASSISTANTS_DIR': str(paths['assistants']), 'TRUSTY_DATA_DIR_OVERRIDE': str(paths['runtime']), 'TRUSTY_SEARCH_SOCKET': str(search_sock), 'TRUSTY_MEMORY_SOCKET': str(memory_sock), 'TRUSTY_MPM_ROOT': str(paths['mpm']), 'TRUSTY_NO_AUTO_DISCOVER': '1', 'TAGENT_NONINTERACTIVE': '1', 'TAGENT_API_TOKEN': api_token, 'OPENROUTER_BASE_URL': 'http://127.0.0.1:' + str(server.server_port) + '/api/v1', 'OLLAMA_HOST': 'http://127.0.0.1:' + str(server.server_port), 'TRUSTY_EMBEDDER': 'in-process', 'TRUSTY_DEVICE': 'cpu', 'RUST_LOG': 'warn'}
    if args.real_vision:
        directory = paths['home'] / '.trusty-tools'
        directory.mkdir()
        credential_link = directory / 'credentials.toml'
        credential_link.symlink_to(args.credential_store.resolve())
    else:
        env['OPENROUTER_API_KEY'] = 'synthetic-loopback-only'
    record('fixture', {'root': str(root), 'assistant_root': str(paths['assistants']), 'real_vision': args.real_vision, 'provider': 'openrouter', 'model': 'openai/gpt-4o-mini', 'token_recorded': False, 'credential_link_only': bool(credential_link)})
    start('memory', args.memory, ['serve', '--foreground'])
    start('search', args.search, ['start', '--foreground', '--data-dir', str(paths['search-data']), '--no-auto-discover', '--device', 'cpu'])
    wait('memory socket', lambda: memory_sock.exists())
    wait('search socket', lambda: search_sock.exists())
    caps = memory_tool('chat_asset_capabilities', {})
    record('real_memory_capabilities', caps)
    assert caps['version'] == 1 and caps['typed_history_attachments'] is True
    start('tagent', args.tagent, ['--api', '--bind', '127.0.0.1', '--port', str(port)])
    wait('API health', lambda: http('GET', '/api/health')['pid'] == children['tagent'].pid)
    image_bytes = synthetic_image()
    (root / 'synthetic-vision.png').write_bytes(image_bytes)
    raw_items = [{'kind': 'file', 'name': 'synthetic-vision.png', 'mime_type': 'image/png', 'data_base64': base64.b64encode(image_bytes).decode()}]
    if not args.real_vision:
        raw_items.append({'kind': 'clipboard', 'name': 'synthetic-table', 'format': 'tsv', 'text': 'Name\tCount\nAtlas\t7'})
    for mode in ['none', 'wrong']:
        http('POST', '/api/chat-attachments/prepare', {'items': raw_items}, 401, auth=mode)
    http('POST', '/api/chat-attachments/prepare', {'items': raw_items}, 403, origin='https://foreign.invalid')
    prepared = http('POST', '/api/chat-attachments/prepare', {'items': raw_items})['attachments']
    assert len(prepared) == len(raw_items)
    assert base64.b64decode(prepared[0]['data_base64']) == image_bytes
    text = 'Name the color and shape on the left, then on the right. Answer with two short phrases.' if args.real_vision else 'Synthetic attachment integration QA: inspect the image and Atlas table.'
    first = task(text, prepared)
    assert_payload(0, image_bytes, prepared[1] if len(prepared) > 1 else None)
    if args.real_vision:
        answer = first['narrative'].lower()
        assert all((w in answer for w in ['red', 'circle', 'blue', 'square'])), answer
    policy = http('GET', '/api/agents/assistant/memory-policy')
    history = http('GET', '/api/agents/assistant/chat-history')
    assert history['available'] and history['palace'] == policy['namespace'] and (history['session_id'] == 'persona-assistant')
    user = next((m for m in history['messages'] if m['role'] == 'user' and m['content'] == text))
    assert len(user['attachments']) == len(prepared)
    asset = user['attachments'][0]
    uuid.UUID(asset['asset_id'])
    assert 'data_base64' not in asset and asset['mime_type'] == 'image/png'
    if len(prepared) > 1:
        assert user['attachments'][1] == prepared[1]
    raw_history = memory_tool('chat_session_get', {'palace': policy['namespace'], 'session_id': 'persona-assistant'})
    assert user in raw_history['history']
    asset_route = '/api/agents/assistant/chat-assets/' + asset['asset_id']
    content, headers = http('GET', asset_route, binary=True)
    assert content == image_bytes and headers.get('content-type') == 'image/png' and (headers.get('x-content-type-options') == 'nosniff') and (headers.get('cache-control') == 'private, no-store')
    for mode in ['none', 'wrong']:
        http('GET', asset_route, status=401, auth=mode)
    http('GET', '/api/agents/qa-attachment-secondary/chat-assets/' + asset['asset_id'], status=404)
    http('GET', '/api/agents/assistant/chat-assets/not-an-id', status=400)
    http('GET', '/api/agents/assistant/chat-assets/' + str(uuid.uuid4()), status=404)
    memory_tool('chat_session_create', {'palace': policy['namespace'], 'session_id': 'qa-other-session', 'title': 'Synthetic foreign-session ownership fixture'})
    foreign_session = memory_tool('chat_asset_put', {'palace': policy['namespace'], 'session_id': 'qa-other-session', 'name': 'other.png', 'mime_type': 'image/png', 'data_base64': base64.b64encode(image_bytes).decode()})
    http('GET', '/api/agents/assistant/chat-assets/' + foreign_session['asset_id'], status=404)
    stop('tagent')
    stop('memory')
    start('memory', args.memory, ['serve', '--foreground'])
    wait('memory restart', lambda: memory_sock.exists())
    start('tagent', args.tagent, ['--api', '--bind', '127.0.0.1', '--port', str(port)])
    wait('API restart', lambda: http('GET', '/api/health')['pid'] == children['tagent'].pid)
    reloaded = http('GET', '/api/agents/assistant/chat-history')
    assert reloaded['messages'] == history['messages']
    assert http('GET', asset_route, binary=True)[0] == image_bytes
    record('attachment_persistence_verified', {'history_namespace': policy['namespace'], 'asset_id': asset['asset_id'], 'image_bytes_equal_after_restart': True, 'typed_table_preserved': len(prepared) > 1, 'foreign_assistant_and_session_denied': True})
    if not args.real_vision:
        previous = len(calls)
        task('Synthetic follow-up: use the previous attached image and table.')
        assert_payload(previous, image_bytes, prepared[1])
        bad = dict(prepared[0])
        bad['data_base64'] = 'invalid'
        http('POST', '/api/task', {'task': 'forged normalized image', 'agent': 'assistant', 'attachments': [bad]}, 400)
        bad = dict(prepared[0])
        del bad['data_base64']
        bad['asset_id'] = asset['asset_id']
        http('POST', '/api/task', {'task': 'forged normalized asset', 'agent': 'assistant', 'attachments': [bad]}, 400)
        before = len(calls)
        before_rejected_history = http('GET', '/api/agents/assistant/chat-history')['messages']
        task('Synthetic unsupported-provider image check.', [prepared[0]], provider='local', model='ollama/fixture', success=False)
        assert len(calls) == before
        after_rejected_history = http('GET', '/api/agents/assistant/chat-history')['messages']
        record('rejected_provider_history', {'before_messages':len(before_rejected_history),'after_messages':len(after_rejected_history),'unchanged':before_rejected_history==after_rejected_history})
        assert before_rejected_history == after_rejected_history, 'Rejected provider persisted a new user turn'
        stop('tagent')
        compat_path = paths['runtime'] / 'old-memory.sock'
        compat_server = UDS(str(compat_path), OldDaemon)
        compat_path.chmod(0o600)
        threading.Thread(target=compat_server.serve_forever, daemon=True).start()
        start('tagent', args.tagent, ['--api', '--bind', '127.0.0.1', '--port', str(port)], {'TRUSTY_MEMORY_SOCKET': str(compat_path)})
        wait('old capability API', lambda: http('GET', '/api/health')['pid'] == children['tagent'].pid)
        before = len(calls)
        failure_result = task('Synthetic old-memory capability check.', prepared, success=False)
        assert len(calls) == before
        assert 'attach' in json.dumps(failure_result).lower() and ('upgrade' in json.dumps(failure_result).lower() or 'support' in json.dumps(failure_result).lower())
        assert any(r['step']=='old_daemon_request' and r['value']['method']=='chat_asset_capabilities' for r in records), 'Capability fixture was not reached'
        record('old_daemon_capability_rejected_before_inference', True)
        control['pass_capabilities'] = True
        control['fail_assets'] = True
        before = len(calls)
        missing = task('Synthetic unavailable historical image.', success=False)
        assert len(calls) == before
        assert 'asset' in json.dumps(missing).lower() or 'memory' in json.dumps(missing).lower()
        record('known_history_asset_failure_rejected_before_inference', True)
        control['fail_assets'] = False
        control['fail_history_probe'] = True
        before = len(calls)
        degraded = task('Synthetic history outage plaintext follow-up.', expected_status='partial')
        assert degraded['errors'] == ['Saved image history and chat persistence are unavailable for this turn. The response may lack earlier image context.']
        assert 'data:image/png;base64,' not in json.dumps(calls[before])
        assert len(calls) > before
        record('plaintext_history_probe_outage', {'status': degraded['status'], 'narrative': degraded.get('narrative'), 'previous_image_sent': 'data:image/png;base64,' in json.dumps(calls[before]), 'task_response': degraded})
    record('integrated_assertions_passed', {'real_vision': args.real_vision, 'provider_requests': len(calls), 'raw_preparation_to_task_to_provider': True, 'typed_memory_history_and_asset_restart': True})
except Exception as e:
    failure = {'error': str(e), 'traceback': traceback.format_exc()}
    record('exception', failure)
finally:
    for name in ['tagent', 'search', 'memory']:
        stop(name)
    if compat_server:
        compat_server.shutdown()
        compat_server.server_close()
    if server:
        server.shutdown()
        server.server_close()
    if credential_link and credential_link.is_symlink():
        credential_link.unlink()
    cleanup_errors = []
    lsof = shutil.which('lsof')
    if lsof:
        output = subprocess.run([lsof, '-n', '-U'], capture_output=True, text=True, timeout=10).stdout
        extra_pids = {int(line.split()[1]) for line in output.splitlines() if str(paths['runtime']) in line and len(line.split()) > 2}
        for pid in extra_pids:
            if pid not in all_pids:
                try:
                    os.kill(pid, 15)
                except ProcessLookupError:
                    pass
                all_pids.append(pid)
                record('stopped_owned_autostart', {'pid': pid})
    for pid in all_pids:
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                break
            time.sleep(0.1)
        else:
            cleanup_errors.append(pid)
    if cleanup_errors:
        failure = {'cleanup_pids_still_alive': cleanup_errors, 'prior_failure': failure}
    record('cleanup_verified', {'owned_pids': all_pids, 'survivors': cleanup_errors, 'credential_link_removed': credential_link is None or not credential_link.exists()})
    for log in logs:
        log.close()
    proof = root / ('real-vision-proof.json' if args.real_vision else 'attachment-integration-proof.json')
    proof.write_text(json.dumps({'records': records, 'failure': failure, 'owned_pids': all_pids, 'credential_link_removed': credential_link is None or not credential_link.exists()}, indent=2))
    print('PROOF=' + str(proof), flush=True)
    if failure:
        raise SystemExit(1)
