"""Bridge the container's loopback to its one mounted model socket."""

import selectors
import socket
import socketserver

PORT = 8765
SOCKET = "/bench-model.sock"


class Relay(socketserver.BaseRequestHandler):
    def handle(self):
        with socket.socket(socket.AF_UNIX) as upstream, selectors.DefaultSelector() as ready:
            upstream.connect(SOCKET)
            self.request.settimeout(5)
            upstream.settimeout(5)
            ready.register(self.request, selectors.EVENT_READ, upstream)
            ready.register(upstream, selectors.EVENT_READ, self.request)
            while ready.get_map():
                for key, _ in ready.select():
                    chunk = key.fileobj.recv(65536)
                    if chunk:
                        key.data.sendall(chunk)
                    else:
                        ready.unregister(key.fileobj)
                        key.data.shutdown(socket.SHUT_WR)


if __name__ == "__main__":
    with socketserver.ThreadingTCPServer(("127.0.0.1", PORT), Relay) as server:
        server.daemon_threads = True
        server.serve_forever()
