// tools.js — celagent 工具集 (Agent 类可调用)
import { Type } from "typebox";

// 工具 1: 获取当前时间
const getTime = {
  name: "get_time",
  label: "获取当前时间",
  description: "获取当前的日期和时间(ISO 格式)",
  parameters: Type.Object({}),
  execute: async () => ({
    content: [{ type: "text", text: new Date().toISOString() }],
  }),
};

// 工具 2: 简单计算器
const calculate = {
  name: "calculate",
  label: "计算器",
  description: "执行简单的数学计算, 支持 + - * / 和括号",
  parameters: Type.Object({
    expression: Type.String({ description: "数学表达式, 如 (1+2)*3" }),
  }),
  execute: async (_id, params) => {
    try {
      // 安全求值: 只允许数字和运算符
      const expr = String(params.expression).replace(/[^0-9+\-*/().\s]/g, "");
      // eslint-disable-next-line no-eval
      const result = Function(`"use strict"; return (${expr})`)();
      return { content: [{ type: "text", text: `${params.expression} = ${result}` }] };
    } catch (e) {
      return { content: [{ type: "text", text: `计算失败: ${e.message}` }] };
    }
  },
};

// 工具 3: 查询 BOS 会话存储 (演示与 Celld 集成)
const sessionGet = {
  name: "session_get",
  label: "读取会话",
  description: "从 Celld 持久化存储读取一个会话的历史轮次",
  parameters: Type.Object({
    session: Type.String({ description: "会话 ID" }),
  }),
  execute: async (_id, params) => {
    try {
      const resp = await fetch(
        `http://127.0.0.1:18090/agent/celagent?action=resume&session=${params.session}`,
        { signal: AbortSignal.timeout(8000) }
      );
      const data = await resp.json();
      const turns = data.session?.turns ?? [];
      const summary = turns.map(t => `#${t.turn}: ${String(t.msg).slice(0, 60)}`).join("\n");
      return {
        content: [{ type: "text", text: `会话 ${params.session}: ${turns.length} 轮\n${summary || "(空)"}` }],
      };
    } catch (e) {
      return { content: [{ type: "text", text: `查询失败: ${e.message}` }] };
    }
  },
};

export const TOOLS = [getTime, calculate, sessionGet];
