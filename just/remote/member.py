"""Drives one estafette member for the lab: serve its MCP session behind a control socket, call one of its tools, or read its inbox socket."""

import json
import signal
import socket
import subprocess
import sys
import time

MCP_PROTOCOL_VERSION = "2025-06-18"
LAB_CLIENT = {"name": "estafette-lab", "version": "1"}
INITIALIZE_ID = 0
FIRST_TOOL_CALL_ID = 1
MILLISECONDS_PER_SECOND = 1000
REPLAY_FROM_THE_FIRST_FRAME = b'{"since": 0}\n'


def unix_milliseconds():
    """Milliseconds since the Unix epoch by this machine's clock."""
    return int(time.time() * MILLISECONDS_PER_SECOND)


def print_stamped(text):
    """Prints the text after the wall clock in milliseconds and a tab."""
    print(f"{unix_milliseconds()}\t{text}", flush=True)


class McpSession:
    """One `estafette mcp` process spoken to over its standard input and output."""

    def __init__(self, binary, network):
        self.process = subprocess.Popen(
            [binary, "mcp", network], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True
        )
        self.next_id = FIRST_TOOL_CALL_ID

    def write(self, message):
        """Writes one JSON-RPC message as one line."""
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()

    def request(self, request_id, method, params):
        """Writes one JSON-RPC request and returns the result, or the error, of the response carrying its id."""
        self.write({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        for line in self.process.stdout:
            response = json.loads(line)
            if response.get("id") == request_id:
                return response.get("result", response.get("error"))
        raise EOFError("the estafette mcp process closed its output")

    def initialize(self):
        """Runs the MCP handshake."""
        self.request(
            INITIALIZE_ID,
            "initialize",
            {"protocolVersion": MCP_PROTOCOL_VERSION, "capabilities": {}, "clientInfo": LAB_CLIENT},
        )
        self.write({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def call_tool(self, name, arguments):
        """Calls one tool and returns its envelope."""
        request_id = self.next_id
        self.next_id += 1
        result = self.request(request_id, "tools/call", {"name": name, "arguments": arguments})
        return result.get("structuredContent", result)

    def stop(self):
        """Closes standard input, which starts the member's clean stop, and returns its exit code."""
        self.process.stdin.close()
        return self.process.wait()


def serve(binary, network, card, control_path):
    """Claims the card, then answers each control connection's one tool call until SIGTERM or SIGINT stops the member cleanly."""
    session = McpSession(binary, network)
    session.initialize()
    print_stamped(json.dumps(session.call_tool("init", {"card": card})))
    for stop_signal in (signal.SIGTERM, signal.SIGINT):
        signal.signal(stop_signal, lambda _signal, _frame: sys.exit(session.stop()))
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as control:
        control.bind(control_path)
        control.listen()
        while True:
            connection, _address = control.accept()
            with connection, connection.makefile("rw") as stream:
                tool_call = json.loads(stream.readline())
                envelope = session.call_tool(tool_call["name"], tool_call["arguments"])
                stream.write(json.dumps(envelope) + "\n")


def call(control_path, name, arguments):
    """Asks the serving member to call one tool and prints its envelope stamped with the wall clock."""
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as control:
        control.connect(control_path)
        with control.makefile("rw") as stream:
            stream.write(json.dumps({"name": name, "arguments": json.loads(arguments)}) + "\n")
            stream.flush()
            print_stamped(stream.readline().rstrip("\n"))


def attach(inbox_path):
    """Reads the inbox socket from its first frame and prints each frame stamped with the wall clock."""
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as inbox:
        inbox.connect(inbox_path)
        inbox.sendall(REPLAY_FROM_THE_FIRST_FRAME)
        for line in inbox.makefile("r"):
            print_stamped(line.rstrip("\n"))


COMMANDS = {"serve": serve, "call": call, "attach": attach}

if __name__ == "__main__":
    COMMANDS[sys.argv[1]](*sys.argv[2:])
