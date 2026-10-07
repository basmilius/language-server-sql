#!/usr/bin/env python3
import argparse
import json
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
import time


def read_messages(stream, messages):
    try:
        while True:
            headers = {}
            while True:
                line = stream.readline()
                if not line:
                    raise EOFError('server closed stdout')
                if line == b'\r\n':
                    break
                key, value = line.decode('ascii').split(':', 1)
                headers[key.lower()] = value.strip()
            length = int(headers['content-length'])
            body = stream.read(length)
            if len(body) != length:
                raise EOFError('server truncated an LSP message')
            messages.put(json.loads(body))
    except Exception as error:
        messages.put(error)


def main():
    parser = argparse.ArgumentParser(description='Check a real SQL server over stdio, without network or a database.')
    parser.add_argument('binary', type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    metadata = json.loads((Path(__file__).resolve().parent.parent / 'native-source.json').read_text())
    version = subprocess.run([str(binary), '--version'], capture_output=True, text=True, check=True, timeout=10)
    if version.stdout.strip() != 'sql-language-server ' + metadata['version']:
        raise AssertionError('binary version differs from native-source.json')

    with tempfile.TemporaryDirectory(prefix='sql-language-server-stdio-') as directory:
        process = subprocess.Popen([str(binary), '--stdio'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, cwd=directory)
        messages = queue.Queue()
        threading.Thread(target=read_messages, args=(process.stdout, messages), daemon=True).start()

        def send(message):
            body = json.dumps({'jsonrpc': '2.0', **message}).encode('utf8')
            process.stdin.write(f'Content-Length: {len(body)}\r\n\r\n'.encode('ascii') + body)
            process.stdin.flush()

        def response(request_id):
            deadline = time.monotonic() + 10
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(f'no response for request {request_id}')
                message = messages.get(timeout=remaining)
                if isinstance(message, Exception):
                    raise message
                if 'id' in message and 'method' in message:
                    send({'id': message['id'], 'result': None})
                    continue
                if message.get('id') == request_id:
                    if 'error' in message:
                        raise AssertionError(message['error'])
                    return message['result']

        try:
            send({'id': 1, 'method': 'initialize', 'params': {'processId': None, 'rootUri': None, 'capabilities': {'general': {'positionEncodings': ['utf-8']}, 'textDocument': {'documentSymbol': {'hierarchicalDocumentSymbolSupport': True}}}}})
            initialized = response(1)
            if initialized['serverInfo'] != {'name': 'sql-language-server', 'version': metadata['version']}:
                raise AssertionError(initialized['serverInfo'])
            if initialized['capabilities']['positionEncoding'] != 'utf-8':
                raise AssertionError('UTF-8 negotiation failed')
            send({'method': 'initialized', 'params': {}})
            uri = (Path(directory) / 'sample.sql').as_uri()
            send({'method': 'textDocument/didOpen', 'params': {'textDocument': {'uri': uri, 'languageId': 'sql', 'version': 1, 'text': 'CREATE TABLE greeting (id int PRIMARY KEY, message text);'}}})
            send({'id': 2, 'method': 'textDocument/documentSymbol', 'params': {'textDocument': {'uri': uri}}})
            symbols = response(2)
            if not any(symbol['name'] == 'greeting' for symbol in symbols):
                raise AssertionError(symbols)
            send({'id': 3, 'method': 'shutdown', 'params': None})
            if response(3) is not None:
                raise AssertionError('shutdown must return null')
            send({'method': 'exit', 'params': None})
            process.stdin.close()
            if process.wait(timeout=10) != 0:
                raise AssertionError(process.stderr.read().decode('utf8'))
            print(f'{metadata["version"]}: stdio initialize, UTF-8, document symbols, shutdown and exit passed')
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=10)


if __name__ == '__main__':
    main()
