// v2/extension.js — Pi 1.5 durability extension
// 在工具执行前写 journal.started；写失败就 block（fail-closed）。
export function durabilityExtension({ getSessionId, journal, onError }) {
  return (pi) => {
    pi.on("tool_call", async (event) => {
      const sessionId = getSessionId();
      if (!sessionId) return;
      try {
        const ok = await journal.start(sessionId, {
          toolCallId: event.toolCallId,
          toolName: event.toolName,
          input: event.input,
        });
        if (!ok) {
          return { block: true, reason: "durable journal unavailable; refusing to run a side effect" };
        }
      } catch (e) {
        if (onError) onError(e);
        return { block: true, reason: `durable journal error: ${e.message}` };
      }
    });

    pi.on("tool_result", async (event) => {
      const sessionId = getSessionId();
      if (!sessionId) return;
      try {
        await journal.finish(sessionId, {
          toolCallId: event.toolCallId,
          result: {
            content: event.content,
            structuredContent: event.structuredContent,
            details: event.details,
          },
          isError: event.isError,
        });
      } catch (e) {
        if (onError) onError(e);
      }
    });
  };
}
