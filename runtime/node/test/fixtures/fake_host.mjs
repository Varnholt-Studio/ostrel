// Minimal stand in for a sidecar host, used only to test the harness itself. Each request's
// `method` selects a behaviour; it implements none of the real host's checks. Like the real host
// it announces itself with a `ready` notification and echoes its command line arguments there.
import { createInterface } from 'node:readline';

const out = (text) => process.stdout.write(text);

out(`${JSON.stringify({ jsonrpc: '2.0', method: 'ready', params: { argv: process.argv.slice(2) } })}\n`);

createInterface({ input: process.stdin }).on('line', (line) => {
  const req = JSON.parse(line);
  switch (req.method) {
    case 'echo':
      out(`${JSON.stringify({ jsonrpc: '2.0', id: req.id, result: req.params })}\n`);
      break;
    case 'split':
      // One response written in three chunks with pauses, to exercise line reassembly.
      out('{"jsonrpc":"2.0",');
      setTimeout(() => out(`"id":${req.id},`), 20);
      setTimeout(() => out('"result":"joined"}\n'), 40);
      break;
    case 'garbage':
      out('this is not json\n');
      break;
    case 'oversized':
      out(`"${'y'.repeat(req.params.bytes)}"\n`);
      break;
    case 'notify':
      out(`${JSON.stringify({ jsonrpc: '2.0', method: 'note', params: req.params })}\n`);
      out(`${JSON.stringify({ jsonrpc: '2.0', id: req.id, result: 'notified' })}\n`);
      break;
    case 'silent':
      break;
    case 'exit':
      process.exit(req.params.code);
      break;
    default:
      break;
  }
});
