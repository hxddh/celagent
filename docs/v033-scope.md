# v0.3.3 范围(下一刀实现)

> 版本号 **`0.3.3`**。依据 `docs/s3-compat-evaluation.md` P0 + `docs/post-v032-evaluation.md`。
> **本文件是实现合同**。实现 PR 已按此落地。
> 默认仍是 BOS;`[bos]` profile / `bj` / `s3.bj.bcebos.com` 用户应无感。

## 用户能感知什么

- 配了非法或不在白名单的 `persistence.endpoint` → **报错退出**,不会再把会话写到百度 BOS。
- `celagent config set persistence.endpoint <合格 URL>` 可以配 AWS S3 / R2 / Tigris host(https)。
- `setup.sh` / `node_mgr.sh` / `cluster_mgr.sh` / `install.sh` 听 `settings.json` 的 endpoint/region/profile,不再无视配置硬编码 BOS。
- `doctor` 打印实际用的 endpoint / region / profile。
- 没改配置的老用户:行为与 v0.3.2 相同。

本版 **不宣称**「已支持 R2/S3」。白名单只让配置进得去;CAS 实测与「已支持」是 v0.3.4。

## 做

### 1. `src/bos.js` — fail-closed + 白名单 + profile/region

| 函数 | 新行为 |
|------|--------|
| `isAllowedEndpoint` | https 白名单扩为:现有 `s3.*.bcebos.com`;`s3.amazonaws.com` / `s3.<region>.amazonaws.com`;`*.r2.cloudflarestorage.com`;`fly.storage.tigris.dev` / `*.tigris.dev` / `t3.storage.dev`。环回 http(s) 仍允许。其它仅当 `CELAGENT_ALLOW_ENDPOINT=1` 且 URL 可解析 |
| `resolveEndpoint(override)` | 无 override → 默认 `https://s3.bj.bcebos.com`。有 override 但不允许 → **抛错或返回失败,禁止返回默认 BOS** |
| `awsEnv(extra)` | 无完整 AK/SK 时 `AWS_PROFILE = extra.AWS_PROFILE \|\| "bos"`。调用方传入 settings 的 profile。仍禁止 env 与 profile 混用 |
| 新增 `defaultRegion(endpoint)` | host 匹配 `*.bcebos.com` → `bj`;否则 `undefined`(调用方必须带 `persistence.region`) |
| `bosPut` / `bosGet` | endpoint 非法则 `{ ok: false, error: "endpoint-not-allowed" }`,不 PUT 到 BOS |

`CELAGENT_ALLOW_ENDPOINT=1` 仍是 escape hatch,但只放行,不改写。

### 2. TUI / 工具读同一份 persistence

命中:`bin/celagent-tui.mjs`、`src/bos-tools.js`。

- 所有 `resolveEndpoint(cfg.persistence?.endpoint)`:捕获失败 → 警告或退出,不静默。
- `config set persistence.endpoint`:非法 → 非零退出,settings 不变(已有检查,改文案提到白名单/env)。
- `ensureCelld --region`:`cfg.persistence?.region \|\| defaultRegion(endpoint)`;非 BOS 且无 region → 拒绝拉起,提示 `config set persistence.region`。
- `listSessions` 不要先写死 BOS URL 再覆盖;默认走 `resolveEndpoint()`。
- `awsEnv()` 传入 `AWS_PROFILE: cfg.persistence?.profile \|\| "bos"`。
- `doctor`:[2/5] 按 settings 的 profile(缺省 `bos`)或 env 查凭证;打印 `endpoint=` `region=` `profile=`。

### 3. 运维脚本读 settings

命中:`setup.sh`、`install.sh`、`scripts/node_mgr.sh`、`scripts/cluster_mgr.sh`、`scripts/celld-bos-test.sh`。

从 `~/.config/celagent/settings.json` 读(jq):

```
bucket    persistence.bucket
endpoint  persistence.endpoint    # 缺省 https://s3.bj.bcebos.com
region    persistence.region      # 缺省: bcebos → bj, 否则必须已配置
profile   persistence.profile     # 缺省 bos
```

`export AWS_PROFILE="$profile"`。`celld --endpoint/--region` 用上面的值。

`create-bucket`:仅当 endpoint 是 `*.bcebos.com` 时沿用现逻辑。其它后端只 `head-bucket`,不存在则失败并提示「请先在控制台建桶」。

`setup.sh` / `install.sh` 写 settings 时:BOS 默认仍写 endpoint+region=bj;若调用前已有非默认 persistence,保留用户值,只补 bucket/token。

### 4. 测试

`tests/review-logic-proofs.test.mjs` 现有用例必须改:

```
resolveEndpoint("https://evil.example")  ≠  BOS URL
→ 断言抛错或 isAllowedEndpoint false 且 resolve 失败
```

补允许用例(不发网):`s3.us-east-1.amazonaws.com`、`*.r2.cloudflarestorage.com`、环回。

`npm test` 全绿。相关脚本 `bash -n`。

### 5. 文档(最小)

- `package.json` version `0.3.3`
- README 配置示例可加一句:非 BOS 需合格 endpoint + 显式 region + 可选 `persistence.profile`;**不要**写「已支持所有 S3」
- HANDOFF / architecture 扩展点 2:去掉「静默退回」,改为 fail-closed
- 本文件开头标明已落地(实现 PR 改状态)

## 明确不进 v0.3.3

- CAS doctor / 真 R2 或 S3 联调 / 「已支持」矩阵
- 删 worker 死代码 SigV4(可顺手,不挡发版)
- rename `bos.js`、GCS、`gs://`、celld 桶 prefix
- 改 BOS 默认 endpoint/profile/region
- 自研 provider 认证、快照 TUI、会话 merge、自动删 `sessions/`
- MinIO/B2 当支持后端
- 把会话权威迁进 celld

## 验收清单

- [ ] `celagent config set persistence.endpoint https://evil.example` → 非零退出,settings 不变
- [ ] 未设 `CELAGENT_ALLOW_ENDPOINT` 时,`resolveEndpoint("https://evil.example")` 不返回 `https://s3.bj.bcebos.com`
- [ ] 零配置(无 persistence.endpoint):默认 BOS + profile `bos` + region `bj`,与 v0.3.2 相同
- [ ] `config set persistence.endpoint https://<account>.r2.cloudflarestorage.com` 成功(有 https 白名单)
- [ ] 非 BOS endpoint 且无 region → ensureCelld / 脚本拒绝启动
- [ ] `node_mgr.sh` 使用 settings 的 endpoint,不再出现脚本内硬编码 `EP="https://s3.bj.bcebos.com"` 作为唯一来源
- [ ] `doctor` 输出含实际 endpoint/region/profile
- [ ] `npm test` + `bash -n install.sh setup.sh scripts/*.sh`

## 命中文件

`src/bos.js` `src/bos-tools.js` `bin/celagent-tui.mjs`
`setup.sh` `install.sh` `scripts/node_mgr.sh` `scripts/cluster_mgr.sh` `scripts/celld-bos-test.sh`
`tests/review-logic-proofs.test.mjs` `package.json` `README.md` `HANDOFF.md` `docs/architecture.md`

## 发版

实现合进 `main` 后打 tag **`v0.3.3`**(不移动 v0.3.0/v0.3.1/v0.3.2)。Release 资产形态不变,随包 celld 仍为 v0.2.0。

✅ 2026-08-13:tag `v0.3.3` → `1514b1b`;Latest 已切;冒烟 `./scripts/release-smoke.sh v0.3.3` 通过。
