# webapp-testing — network interception, debugging and parallel execution

## Network Interception

### Mock API Responses
```python
# Intercept and mock API response
def handle_route(route):
    route.fulfill(
        status=200,
        body='{"success": true, "data": "mocked"}',
        headers={'Content-Type': 'application/json'}
    )

page.route('**/api/data', handle_route)
page.goto('http://localhost:3000')
```

### Block Resources
```python
# Block images and stylesheets for faster tests
page.route('**/*.{png,jpg,jpeg,gif,svg,css}', lambda route: route.abort())
```

### Wait for Network Responses
```python
# Wait for specific API call
with page.expect_response('**/api/users') as response_info:
    page.click('button.load-users')
response = response_info.value
assert response.status == 200
```

## Debugging

### Pause Execution
```python
page.pause()  # Opens Playwright Inspector
```

### Console Logs
```python
def handle_console(msg):
    print(f"[{msg.type}] {msg.text}")

page.on("console", handle_console)
```

### Slow Motion
```python
browser = p.chromium.launch(headless=False, slow_mo=1000)  # 1 second delay
```

### Verbose Logging
```python
# Set DEBUG environment variable
# DEBUG=pw:api python test.py
```

## Parallel Execution

### Pytest Parallel
```bash
# Install pytest-xdist
pip install pytest-xdist

# Run tests in parallel
pytest -n auto  # Auto-detect CPU cores
pytest -n 4     # Run with 4 workers
```

### Browser Context Isolation
```python
# Each test gets isolated context (cookies, localStorage, etc.)
@pytest.fixture
def context(browser):
    context = browser.new_context()
    yield context
    context.close()

@pytest.fixture
def page(context):
    return context.new_page()
```
