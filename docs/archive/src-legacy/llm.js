// llm.js — 真实 LLM 集成 (pi-ai openai-completions + deepseek)
// key 从环境变量 DEEPSEEK_API_KEY 读取 (不碰 ~/.pi)
import { streamSimple } from "@earendil-works/pi-ai/api/openai-completions";

// deepseek Model 定义 (OpenAI 兼容)
const DEEPSEEK_MODEL = {
  id: "deepseek-chat",
  name: "DeepSeek Chat",
  api: "openai-completions",
  provider: "deepseek",
  baseUrl: "https://api.deepseek.com/v1",
  reasoning: false,
  input: ["text"],
  cost: { input: 0.14, output: 0.28, cacheRead: 0.07, cacheWrite: 0.14, tiers: [] },
  contextWindow: 64000,
  maxTokens: 8192,
};

// 简易 context 构造 (Agent 内部会做, 这里独立用)
function buildContext(messages, tools = []) {
  return {
    messages,
    tools,
    toolChoice: undefined,
    transforms: [],
  };
}

// 调真实 LLM, 返回回复文本
export async function llmReply(prompt, history = [], { model = "deepseek-chat" } = {}) {
  const apiKey = process.env.DEEPSEEK_API_KEY;
  if (!apiKey) {
    throw new Error("未设置 DEEPSEEK_API_KEY 环境变量 (export DEEPSEEK_API_KEY=sk-xxx)");
  }

  const current = { ...DEEPSEEK_MODEL, id: model };
  // pi-ai 消息格式: content 是 blocks 数组
  const textBlock = (t, role = "user") => ({ role, content: [{ type: "text", text: t }] });
  const messages = [
    { role: "system", content: [{ type: "text", text: "你是 celagent, 一个轻量高效的中文 AI agent。" }] },
    // history 保留原始 role (user/assistant), 提供真实多轮上下文
    ...history.map(h => textBlock(h.content, h.role === "assistant" ? "assistant" : "user")),
    textBlock(prompt),
  ];

  const context = buildContext(messages);
  let replyText = "";
  let usage = null;

  // streamSimple 返回事件流
  const stream = await streamSimple(current, context, { apiKey });
  for await (const event of stream) {
    if (event.type === "text_delta") {
      replyText += event.delta;
    } else if (event.type === "done") {
      usage = event.usage;
    } else if (event.type === "error" || (event.type === "done" && event.message?.stopReason === "error")) {
      const errMsg = event.type === "error" ? JSON.stringify(event) : (event.message?.errorMessage ?? "unknown");
      throw new Error(`LLM 调用失败: ${errMsg}`);
    }
  }

  return { text: replyText.trim(), usage };
}

// Agent 类用的 StreamFn: (model, context, options) => 事件流
// 内部复用 streamSimple (deepseek OpenAI 兼容)
export function buildStreamFn({ model = "deepseek-chat", apiKey } = {}) {
  const current = { ...DEEPSEEK_MODEL, id: model };
  return async (mdl, ctx, options = {}) => {
    const key = apiKey || process.env.DEEPSEEK_API_KEY;
    if (!key) {
      // 错误编码进事件流 (Agent 契约要求不 throw)
      return (async function* () {
        yield {
          type: "done",
          message: { role: "assistant", content: [], stopReason: "error", errorMessage: "未设置 DEEPSEEK_API_KEY" },
          usage: { inputTokens: 0, outputTokens: 0, totalTokens: 0, costUSD: 0 },
        };
      })();
    }
    return streamSimple(current, ctx, { apiKey: key, ...options });
  };
}
