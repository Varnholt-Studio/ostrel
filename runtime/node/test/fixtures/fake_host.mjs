// Minimal stand in for a sidecar host, used only to test the harness itself. Each request's
// `method` selects a behaviour; it implements none of the real host's checks.
import { createInterface } from 'node:readline';

const out = (text) => process.stdout.write(text);

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
    case 'silent':
      break;
    case 'exit':
      process.exit(req.params.code);
      break;
    default:
      break;
  }
});
