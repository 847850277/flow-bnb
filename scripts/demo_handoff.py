#!/usr/bin/env python3
"""Offline MCP -> operator -> wallet adapter -> mocked RPC settlement demo.
No private keys, Binance credentials, public-chain requests or real transactions.
--self-test supplies operator input ONLY to this private mock fixture via a PTY.
"""
import argparse
import json
import os
from pathlib import Path
import pty
import re
import select
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = Path(__file__).resolve().parents[1]
WALLET = '0x' + '1' * 40
SELL = '0x' + '2' * 40
BUY = '0x' + '3' * 40
ROUTER = '0x' + '4' * 40
TX = '0x' + 'a' * 64
BLOCK = '0x' + 'b' * 64


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--self-test', action='store_true')
    parser.add_argument('--no-build', action='store_true')
    parser.add_argument('--scenario', choices=['success', 'decline', 'wallet-reject', 'timeout', 'expired', 'wrong-chain', 'cancel'], default='success')
    parser.add_argument('--output', type=Path, help='Save simulation evidence (must not already exist)')
    args = parser.parse_args()
    if not args.no_build:
        subprocess.run(['cargo', 'build', '--locked', '--bins'], cwd=ROOT, check=True)
    print('SIMULATION ONLY: mock API and mock RPC, no signing keys or mainnet transactions.', flush=True)
    state = {'sent': 0, 'tx': None, 'methods': []}

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            assert not any(name.lower().startswith('x-oc-') for name in self.headers)
            method = body['method']
            state['methods'].append(method)
            error = None
            result = None
            if method == 'web3_clientVersion':
                result = 'flow-bnb-mock/1.0'
            elif method == 'eth_chainId':
                result = '0x1' if args.scenario == 'wrong-chain' else '0x38'
            elif method == 'eth_accounts':
                result = [WALLET]
            elif method == 'eth_estimateGas':
                result = '0x186a0'
            elif method == 'eth_gasPrice':
                result = '0x3b9aca00'
            elif method == 'eth_sendTransaction':
                if args.scenario == 'wallet-reject':
                    error = {'code': 4001, 'message': 'User rejected fixture transaction'}
                else:
                    state['sent'] += 1
                    state['tx'] = body['params'][0]
                    assert state['tx']['from'] == WALLET
                    assert state['tx']['to'] == ROUTER
                    assert state['tx']['value'] == '0x0'
                    assert state['tx']['data'] == '0x12345678'
                    result = TX
                    if args.scenario == 'timeout':
                        time.sleep(4)  # Deliberately ambiguous: fixture accepted before timeout.
            elif method == 'eth_getTransactionByHash':
                assert state['tx'] is not None
                result = dict(state['tx'], hash=TX, chainId='0x38', input=state['tx']['data'])
            elif method == 'eth_getTransactionReceipt':
                result = {'transactionHash': TX, 'blockHash': BLOCK, 'blockNumber': '0x10', 'status': '0x1', 'gasUsed': '0x186a0'}
            elif method == 'eth_blockNumber':
                result = '0x12'
            elif method == 'eth_getBlockByNumber':
                result = {'number': '0x10', 'hash': BLOCK}
            else:
                error = {'code': -32601, 'message': 'Unsupported fixture method'}
            reply = {'jsonrpc': '2.0', 'id': body['id']}
            reply.update({'error': error} if error else {'result': result})
            payload = json.dumps(reply).encode()
            try:
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
            except (BrokenPipeError, ConnectionResetError):
                pass

    with tempfile.TemporaryDirectory(prefix='flow-bnb-demo-') as temporary:
        directory = Path(temporary)
        inbox = directory / 'inbox'
        inbox.mkdir(mode=0o700)
        node = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        worker = threading.Thread(target=node.serve_forever, daemon=True)
        worker.start()
        rpc_url = 'http://127.0.0.1:' + str(node.server_port)
        policy = {'risk': {'max_notional_usd': 10, 'allowed_token_addresses': [SELL, BUY]}, 'allowed_routers': [ROUTER], 'allowed_spenders': [], 'max_age_seconds': 1 if args.scenario == 'expired' else 2 if args.scenario == 'timeout' else 30}
        wallet = {'development_only': True, 'rpc_url': rpc_url, 'account': WALLET, 'max_gas': 200000, 'max_gas_price_wei': 2000000000, 'max_fee_wei': '400000000000000'}
        (directory / 'policy.json').write_text(json.dumps(policy))
        (directory / 'wallet.json').write_text(json.dumps(wallet))
        request = {'wallet_address': WALLET, 'from_token_address': SELL, 'to_token_address': BUY, 'amount': '5000000000000000000', 'slippage_bps': 50}
        env = os.environ.copy()
        env['FLOW_BNB_HANDOFF_DIR'] = str(inbox)
        env.pop('BINANCE_WEB3_API_KEY', None)
        env.pop('BINANCE_WEB3_SECRET_KEY', None)
        client = subprocess.Popen([str(ROOT / 'target/debug/flow-bnb-mcp'), '--root', str(ROOT)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)
        seq = 0

        def mcp(method, params, notification=False):
            nonlocal seq
            seq += 1
            message = {'jsonrpc': '2.0', 'method': method, 'params': params}
            if not notification:
                message['id'] = seq
            client.stdin.write(json.dumps(message) + '\n')
            client.stdin.flush()
            if notification:
                return None
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if not select.select([client.stdout], [], [], max(0, deadline - time.monotonic()))[0]:
                    break
                line = client.stdout.readline()
                if not line:
                    raise RuntimeError('MCP server exited')
                response = json.loads(line)
                if response.get('id') == seq:
                    if 'error' in response:
                        raise RuntimeError(str(response['error']))
                    return response['result']
            raise TimeoutError('MCP response timed out')

        def call(name, arguments):
            result = mcp('tools/call', {'name': name, 'arguments': arguments})
            if result.get('isError'):
                raise RuntimeError(str(result))
            return result.get('structuredContent') or json.loads(next(c['text'] for c in result['content'] if c['type'] == 'text'))

        try:
            mcp('initialize', {'protocolVersion': '2024-11-05', 'capabilities': {}, 'clientInfo': {'name': 'offline-fixture-client', 'version': '1'}})
            mcp('notifications/initialized', {}, notification=True)
            tools = mcp('tools/list', {})['tools']
            names = {tool['name'] for tool in tools}
            assert {'request_trade_execution', 'get_trade_execution', 'cancel_trade_execution'} <= names
            assert 'approve_trade' not in names
            queued = call('request_trade_execution', request)
            intent_id = queued['intent']['id']
            assert queued['status']['state'] == 'awaiting_operator'
            command = [str(ROOT / 'target/debug/flow-bnb'), 'approve-trade', '--handoff-dir', str(inbox), '--intent-id', intent_id, '--policy', str(directory / 'policy.json'), '--wallet-config', str(directory / 'wallet.json'), '--demo']
            transcript = ''
            if args.scenario == 'cancel':
                assert call('cancel_trade_execution', {'intent_id': intent_id})['state'] == 'cancelled'
                return_code = None
            elif args.self_test:
                # The test owns the PTY and the isolated mock RPC endpoint. No public-chain config is accepted.
                pid, fd = pty.fork()
                if pid == 0:
                    os.execve(command[0], command, env)
                answered = False
                deadline = time.monotonic() + 20
                try:
                    while time.monotonic() < deadline:
                        if not select.select([fd], [], [], 0.1)[0]:
                            continue
                        try:
                            part = os.read(fd, 65536)
                        except OSError:
                            break
                        if not part:
                            break
                        transcript += part.decode(errors='replace')
                        if not answered and 'Type its complete confirmation_id' in transcript:
                            confirmation = re.search(r'"confirmation_id":\s*"([a-f0-9]{64})"', transcript).group(1)
                            if args.scenario == 'expired':
                                time.sleep(1.2)
                            answer = 'cancel' if args.scenario == 'decline' else confirmation
                            os.write(fd, (answer + '\n').encode())
                            answered = True
                    else:
                        raise TimeoutError('operator fixture timed out')
                    _, status = os.waitpid(pid, 0)
                    return_code = os.waitstatus_to_exitcode(status)
                finally:
                    os.close(fd)
                    try:
                        os.kill(pid, 9)
                    except ProcessLookupError:
                        pass
            else:
                print('MCP queued intent:', intent_id, flush=True)
                print('Starting operator CLI; inspect the simulated trade and confirm or cancel.', flush=True)
                return_code = subprocess.run(command, env=env).returncode
            status = call('get_trade_execution', {'intent_id': intent_id})
            audit_path = inbox / (intent_id + '.audit.json')
            audit = json.loads(audit_path.read_text()) if audit_path.exists() else None
            if args.self_test or args.scenario == 'cancel':
                expected = {'success': 'completed_simulation', 'decline': 'not_submitted', 'expired': 'not_submitted', 'wallet-reject': 'needs_attention_outcome_may_be_unknown', 'wrong-chain': 'needs_attention_outcome_may_be_unknown', 'timeout': 'needs_attention_outcome_may_be_unknown', 'cancel': 'cancelled'}[args.scenario]
                assert status['state'] == expected, (status, transcript)
                assert state['sent'] == (1 if args.scenario in ('success', 'timeout') else 0), state
                if args.scenario == 'success':
                    assert return_code == 0
                    assert audit['mode'] == 'simulation'
                    assert audit['settlement']['state'] == 'confirmed_balances_observed'
                    assert status['result']['evidence_kind'] == 'simulation'
                    assert status['result']['tx_hash'] == TX
                    assert status['result']['receipt_success'] is True
                    assert status['result']['sell_delta'] == '-5000000000000000000'
                    assert status['result']['buy_delta'] == '1000000'
                # A second actor cannot cancel/claim a consumed intent.
                replay = mcp('tools/call', {'name': 'cancel_trade_execution', 'arguments': {'intent_id': intent_id}})
                assert replay.get('isError'), replay
            evidence = {'mode': 'SIMULATION_ONLY', 'scenario': args.scenario, 'mcp_status': status, 'mock_submissions': state['sent'], 'rpc_methods': state['methods'], 'audit': audit}
            if args.output:
                with args.output.open('x') as output:
                    json.dump(evidence, output, indent=2)
            print(json.dumps({'mode': evidence['mode'], 'scenario': args.scenario, 'state': status['state'], 'mock_submissions': state['sent']}, indent=2))
        finally:
            client.terminate()
            try:
                client.wait(timeout=3)
            except subprocess.TimeoutExpired:
                client.kill()
                client.wait()
            node.shutdown()
            node.server_close()


if __name__ == '__main__':
    main()
