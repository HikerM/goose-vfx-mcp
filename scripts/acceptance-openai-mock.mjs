import http from 'node:http';
import fs from 'node:fs';

const host = '127.0.0.1';
const port = Number(process.env.LUMINA_ACCEPTANCE_MOCK_PORT || '19764');
const logPath =
  process.env.LUMINA_ACCEPTANCE_MOCK_LOG ||
  new URL('../target/acceptance-provider.log', import.meta.url);

function writeLog(entry) {
  if (!logPath) return;
  fs.appendFileSync(logPath, `${JSON.stringify(entry)}\n`, 'utf8');
}

function sendJson(response, status, body) {
  response.writeHead(status, {
    'content-type': 'application/json; charset=utf-8',
    'cache-control': 'no-store',
  });
  response.end(JSON.stringify(body));
}

const server = http.createServer((request, response) => {
  const chunks = [];
  request.on('data', (chunk) => chunks.push(chunk));
  request.on('end', () => {
    const rawBody = Buffer.concat(chunks).toString('utf8');
    let body = null;
    if (rawBody) {
      try {
        body = JSON.parse(rawBody);
      } catch {
        sendJson(response, 400, { error: { message: 'invalid JSON' } });
        return;
      }
    }

    writeLog({
      at: new Date().toISOString(),
      method: request.method,
      url: request.url,
      model: body?.model ?? null,
      messageCount: Array.isArray(body?.messages) ? body.messages.length : 0,
      tools: Array.isArray(body?.tools)
        ? body.tools.map((tool) => tool?.function?.name).filter(Boolean)
        : [],
    });

    if (request.method === 'GET' && request.url === '/health') {
      sendJson(response, 200, { status: 'ok' });
      return;
    }

    if (request.method === 'GET' && request.url === '/v1/models') {
      sendJson(response, 200, {
        object: 'list',
        data: [
          {
            id: 'lumina-mock',
            object: 'model',
            owned_by: 'lumina-acceptance',
            meta: { n_ctx: 32768 },
          },
        ],
      });
      return;
    }

    if (request.method === 'POST' && request.url === '/v1/chat/completions') {
      const messages = Array.isArray(body?.messages) ? body.messages : [];
      const latestUserMessage = [...messages]
        .reverse()
        .find((message) => message?.role === 'user')?.content;
      const toolResult = [...messages].reverse().find((message) => message?.role === 'tool');
      const acceptanceTool = Array.isArray(body?.tools)
        ? body.tools.find((tool) =>
            String(tool?.function?.name ?? '').endsWith('lumina_acceptance_echo')
          )
        : null;

      if (
        typeof latestUserMessage === 'string' &&
        latestUserMessage.includes('调用 MCP 验收工具') &&
        acceptanceTool &&
        !toolResult
      ) {
        sendJson(response, 200, {
          id: 'lumina-acceptance-tool-request',
          object: 'chat.completion',
          created: Math.floor(Date.now() / 1000),
          model: body?.model ?? 'lumina-mock',
          choices: [
            {
              index: 0,
              message: {
                role: 'assistant',
                content: null,
                tool_calls: [
                  {
                    id: 'call_lumina_acceptance',
                    type: 'function',
                    function: {
                      name: acceptanceTool.function.name,
                      arguments: JSON.stringify({ text: 'from installed Lumina' }),
                    },
                  },
                ],
              },
              finish_reason: 'tool_calls',
            },
          ],
          usage: { prompt_tokens: 8, completion_tokens: 5, total_tokens: 13 },
        });
        return;
      }

      if (toolResult) {
        sendJson(response, 200, {
          id: 'lumina-acceptance-tool-result',
          object: 'chat.completion',
          created: Math.floor(Date.now() / 1000),
          model: body?.model ?? 'lumina-mock',
          choices: [
            {
              index: 0,
              message: {
                role: 'assistant',
                content: `Lumina MCP tool call acceptance OK | ${toolResult.content}`,
              },
              finish_reason: 'stop',
            },
          ],
          usage: { prompt_tokens: 12, completion_tokens: 9, total_tokens: 21 },
        });
        return;
      }

      sendJson(response, 200, {
        id: 'lumina-acceptance-response',
        object: 'chat.completion',
        created: Math.floor(Date.now() / 1000),
        model: body?.model ?? 'lumina-mock',
        choices: [
          {
            index: 0,
            message: {
              role: 'assistant',
              content: 'Lumina provider acceptance OK',
            },
            finish_reason: 'stop',
          },
        ],
        usage: {
          prompt_tokens: 8,
          completion_tokens: 5,
          total_tokens: 13,
        },
      });
      return;
    }

    sendJson(response, 404, { error: { message: 'not found' } });
  });
});

server.listen(port, host, () => {
  writeLog({ at: new Date().toISOString(), event: 'listening', host, port });
});
