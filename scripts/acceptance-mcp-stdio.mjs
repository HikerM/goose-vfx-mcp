import { appendFileSync } from 'node:fs';
import { createInterface } from 'node:readline';

const logPath = 'D:\\Project\\goose\\target\\acceptance-mcp.log';

function log(event, details = {}) {
  appendFileSync(logPath, `${JSON.stringify({ at: new Date().toISOString(), event, ...details })}\n`);
}

function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

function result(id, value) {
  send({ jsonrpc: '2.0', id, result: value });
}

const input = createInterface({ input: process.stdin, crlfDelay: Infinity });

log('started', { pid: process.pid });

input.on('line', (line) => {
  if (!line.trim()) return;

  let request;
  try {
    request = JSON.parse(line);
  } catch (error) {
    log('invalid_json', { message: String(error) });
    return;
  }

  log('request', { method: request.method, id: request.id ?? null });

  if (request.method === 'initialize') {
    result(request.id, {
      protocolVersion: request.params?.protocolVersion ?? '2025-03-26',
      capabilities: { tools: {} },
      serverInfo: { name: 'lumina-acceptance-mcp', version: '1.0.0' },
    });
    return;
  }

  if (request.method === 'tools/list') {
    result(request.id, {
      tools: [
        {
          name: 'lumina_acceptance_echo',
          description: 'Returns a deterministic Lumina MCP acceptance response.',
          inputSchema: {
            type: 'object',
            properties: { text: { type: 'string' } },
            required: ['text'],
            additionalProperties: false,
          },
        },
      ],
    });
    return;
  }

  if (request.method === 'tools/call') {
    const text = request.params?.arguments?.text ?? '';
    result(request.id, {
      content: [{ type: 'text', text: `Lumina MCP acceptance OK: ${text}` }],
      isError: false,
    });
    return;
  }

  if (request.method === 'ping') {
    result(request.id, {});
    return;
  }

  if (request.id !== undefined) {
    send({
      jsonrpc: '2.0',
      id: request.id,
      error: { code: -32601, message: `Method not found: ${request.method}` },
    });
  }
});

input.on('close', () => log('stopped'));
