// PaintHarness — the test endpoints the view is pointed at.
//
// Split from `main.swift` for the 500-SLOC cap (#7856); that file's header
// carries the Why/What/Test for the whole harness.

import Foundation
import Network

// MARK: - Test endpoints

/// A port nothing is listening on: bind an ephemeral one, read it, release it.
/// Racy in principle, unreachable in practice on a loopback-only test host, and
/// far safer than hardcoding a number some other service may hold.
func closedPort() -> Int {
    let listener: NWListener
    do {
        listener = try NWListener(using: .tcp, on: .any)
    } catch {
        note("could not bind an ephemeral port: \(error)")
        finish(6)
    }
    let ready = DispatchSemaphore(value: 0)
    listener.stateUpdateHandler = { if case .ready = $0 { ready.signal() } }
    listener.newConnectionHandler = { $0.cancel() }
    listener.start(queue: .global())
    _ = ready.wait(timeout: .now() + 5)
    let port = Int(listener.port?.rawValue ?? 0)
    listener.cancel()
    guard port > 0 else {
        note("ephemeral listener never reported a port")
        finish(6)
    }
    return port
}

/// A listener that completes the TCP handshake and then says nothing — the
/// shape of a daemon that has bound its socket during a restart but cannot yet
/// answer an HTTP request. Counts every connection it accepts, which is how the
/// harness sees the view give up and retry.
///
/// `hangsUp: true` closes each connection instead of holding it, so the load
/// fails at once rather than stalling. #7846's `occluded-failing` mode needs
/// hard failures it can also COUNT, which a closed port cannot give it.
final class SilentListener {
    private let listener: NWListener
    private let lock = NSLock()
    private var connections: [NWConnection] = []
    private var count = 0
    private let hangsUp: Bool

    /// Defaulted rather than assigned once at the end, because the connection
    /// handler below captures `self` and Swift will not allow that until every
    /// stored property holds a value.
    private(set) var port = 0

    init?(hangsUp: Bool = false) {
        guard let listener = try? NWListener(using: .tcp, on: .any) else { return nil }
        self.listener = listener
        self.hangsUp = hangsUp
        let ready = DispatchSemaphore(value: 0)
        listener.stateUpdateHandler = { if case .ready = $0 { ready.signal() } }
        listener.newConnectionHandler = { [weak self] connection in
            guard let self else { return }
            self.lock.lock()
            self.count += 1
            // Held so ARC does not release the connection and close the socket,
            // which would look to the client like a refusal rather than a stall.
            if !self.hangsUp { self.connections.append(connection) }
            self.lock.unlock()
            guard !self.hangsUp else {
                // Started and then closed, not merely cancelled: an accepted
                // connection that is dropped is a hard, fast failure at the
                // client, where an unstarted cancel can present as the stall
                // this listener's other mode exists to produce.
                connection.start(queue: .global())
                DispatchQueue.global().asyncAfter(deadline: .now() + 0.05) { connection.cancel() }
                return
            }
            connection.start(queue: .global())
        }
        listener.start(queue: .global())
        guard ready.wait(timeout: .now() + 5) == .success,
              let bound = listener.port?.rawValue, bound > 0 else {
            listener.cancel()
            return nil
        }
        port = Int(bound)
    }

    var accepted: Int {
        lock.lock()
        defer { lock.unlock() }
        return count
    }

    func stop() {
        lock.lock()
        connections.forEach { $0.cancel() }
        connections.removeAll()
        lock.unlock()
        listener.cancel()
    }
}

/// A listener that actually ANSWERS, so the view reaches `.live` — the state
/// both halves of #7112 happen in, and the one `SilentListener` cannot produce.
///
/// The page it serves reports `document.visibilityState === 'visible'` to its
/// first `visibleReads` reads and `'hidden'` to every read after them. That is
/// the transition WebKit performs when RunningBoard marks the WebContent process
/// NotVisible and the layer trees are frozen, reproduced without needing the OS
/// to do it: the saver's probe is the only reader, so the flip is deterministic
/// rather than timed, and counting reads is what lets the harness demand the
/// view leave a HEALTHY page alone for a stretch first.
///
/// `visibleReads: 0` serves a page that is hidden from its very first answer —
/// the case with no visible-then-hidden history to reason from, which the view
/// must still recover because its own window is not occluded.
///
/// Every response is held for [`suspendResponseDelay`] — see that constant.
/// Only requests for the console path are counted, so a favicon or any other
/// incidental fetch cannot be mistaken for a reload.
final class PageListener {
    private let listener: NWListener
    private let lock = NSLock()
    private var count = 0

    private(set) var port = 0

    /// Requests the view has made for the console path.
    var documentRequests: Int {
        lock.lock()
        defer { lock.unlock() }
        return count
    }

    private let body: String

    static func page(visibleReads: Int) -> String {
        """
        <!doctype html><html><head><meta charset="utf-8"><title>suspend harness</title>
        <style>html,body{margin:0;height:100%;background:#201612;color:#f0e7d8;\
        font:48px monospace;display:flex;align-items:center;justify-content:center}</style>
        </head><body><div>suspend harness</div><script>
        var probes = 0;
        Object.defineProperty(document, 'visibilityState', {
          configurable: true,
          get: function () {
            probes += 1;
            return probes <= \(visibleReads) ? 'visible' : 'hidden';
          }
        });
        </script></body></html>
        """
    }

    init?(visibleReads: Int) {
        guard let listener = try? NWListener(using: .tcp, on: .any) else { return nil }
        self.listener = listener
        self.body = Self.page(visibleReads: visibleReads)
        let ready = DispatchSemaphore(value: 0)
        listener.stateUpdateHandler = { if case .ready = $0 { ready.signal() } }
        listener.newConnectionHandler = { [weak self] connection in
            connection.start(queue: .global())
            self?.serve(connection)
        }
        listener.start(queue: .global())
        guard ready.wait(timeout: .now() + 5) == .success,
              let bound = listener.port?.rawValue, bound > 0 else {
            listener.cancel()
            return nil
        }
        port = Int(bound)
    }

    /// One request, one response, one close. A loopback GET arrives in a single
    /// segment, so this reads once rather than accumulating a full header block.
    private func serve(_ connection: NWConnection) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, _, _ in
            guard let self else { return }
            let request = data.flatMap { String(data: $0, encoding: .utf8) } ?? ""
            guard request.contains(" /ui/screensaver") else {
                connection.cancel()
                return
            }
            self.lock.lock()
            self.count += 1
            self.lock.unlock()

            let payload = Data(self.body.utf8)
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n"
                + "Content-Length: \(payload.count)\r\nConnection: close\r\n\r\n"
            DispatchQueue.global().asyncAfter(deadline: .now() + suspendResponseDelay) {
                connection.send(content: Data(head.utf8) + payload,
                                completion: .contentProcessed { _ in connection.cancel() })
            }
        }
    }

    func stop() { listener.cancel() }
}
