# Codex 机密医疗助理

用 Codex 自主阅读体检与门诊资料、比较变化、编写分析代码并整理就诊问题。
资料保存在可信 Trustee；收到分析任务后，Codex 按 Skill 直接调用 CDH
命令行取数。Pilot 使用官方版本和现有纳管协议。

`trustee-data/` 中的人物、机构、日期和检验数值均为合成示例。
它们不是临床参考资料，不能用于真实诊断或治疗。

## 文件与部署位置

| 文件 | 用途 | 部署位置 |
|---|---|---|
| `skills/medical-assistant/SKILL.md` | 取数方法与医疗资料整理指导 | Codex 的 `.agents/skills/medical-assistant/` |
| `trustee-data/catalog.json` | 可按需获取的文档目录 | Trustee 资源 `medical-catalog` |
| `trustee-data/exam-*` | 三个年度的模拟体检资料 | Trustee 中对应的无扩展名资源 |
| `trustee-data/visit-*` | 两次模拟门诊记录 | Trustee 中对应的无扩展名资源 |

复用 [Codex 部署示例](../codex/README.md)，设置 service ID 为
`medical-codex`，并按 [Trustee 文档](../../docs/trustee.md) 配置可信 Trustee、
参考值和运行环境授权。使用支持 Trustee 的 cagent 版本。

在 Codex 示例的 `build.packages` 中增加 `python3` 和 `findutils`，并把 Skill
加入 `build.files`。`findutils` 也是 Node/Pilot 安装流程使用的基础工具。模型凭据和纳管配置可作为
启动资源；**医学资料不加入 `build.files` 或 AppSpec `resources`**，避免镜像
携带明文或 daemon 在启动时主动下载。Skill 中的目录 URI 与本文 service ID 对应；
更换 service ID 时，同时更新目录中各 URI，并设置 `MEDICAL_CATALOG_URI`。

中文分析需要 `C.UTF-8` locale。镜像裁剪时保留 `/usr/lib/locale/C.utf8/`；
已裁掉该目录的 Guest 可用与已安装版本相同的 `glibc-common` RPM 恢复：

```bash
rpm -Uvh --replacepkgs /path/to/glibc-common-matching-version.rpm
```

## 在可信 Trustee 上准备资料

由数据方管理 Trustee。将 `trustee-data/` 复制到 Trustee 管理机，然后使用
该实例实际配置的 LocalFs repository 路径。下面对应 stock Trustee 的
`/opt/trustee/kbs/repository`：

```bash
install -d -m 0700 /opt/trustee/kbs/repository/medical-codex/local-resources
install -m 0600 trustee-data/catalog.json \
  /opt/trustee/kbs/repository/medical-codex/local-resources/medical-catalog
for file in trustee-data/exam-* trustee-data/visit-*; do
  name=${file##*/}
  install -m 0600 "$file" \
    "/opt/trustee/kbs/repository/medical-codex/local-resources/${name%.*}"
done
```

沿用 confidential-agent 配置的 Worker 运行环境授权，不增加任务级授权。
Trustee 管理私钥留在可信部署机，不交给 Codex。

## 在 Guest 中配置原生 CDH 命令

在 Trustee 模式的 Guest 内，以运行 Codex 的账号准备配置。以下示例在 root
账号下执行，复用 daemon 已生成的 `/run/cai/trustee-runtime.json`。
使用 AA/CDH 1.7.1；旧版 CDH 1.5.1 不支持这里的 AA socket 配置。

当前 CDH 的 KBS URL 不会自动配置 Attestation Agent 的 GetToken 目标。
因此启动一个配置好 Trustee 地址的原生 AA 进程，为 CDH 提供 token。
它不初始化 event log，也不替换 Guest 的共享 AA。

```bash
umask 077
install -d -m 0700 /run/cai/medical
jq '{
  token_configs: {kbs: {url: .kbs_url, cert: .kbs_ca_cert}},
  eventlog_config: {init_pcr: 17, enable_eventlog: false},
  aa_instance: {heartbeat: {enabled: false}}
}' /run/cai/trustee-runtime.json > /run/cai/medical/aa.json

jq '{
  credentials: [],
  kbc: ({name: "cc_kbc", url: .kbs_url} +
    (if .kbs_ca_cert then {kbs_cert: .kbs_ca_cert} else {} end)),
  socket: "unix:///run/cai/medical/cdh.sock",
  aa: {aa_socket: "unix:///run/cai/medical/aa.sock"}
}' /run/cai/trustee-runtime.json > /run/cai/medical/cdh.json

systemd-run --unit=cai-medical-aa \
  --setenv="TRUSTEE_URL=$(jq -r .kbs_url /run/cai/trustee-runtime.json)" \
  /usr/bin/attestation-agent \
  --config-file /run/cai/medical/aa.json \
  --attestation_sock unix:///run/cai/medical/aa.sock
```

这些是运行时配置和 transient service；Guest 重启后重新执行本节命令。
本节不会获取任何医学文档。后续取数直接使用 Skill 中的 CDH 命令，stdout
经 Base64 解码后写入任务目录。

## 发起分析

将 Skill 放入 Codex 实际工作目录的 `.agents/skills/medical-assistant/`，或
该运行账号的用户级技能目录。仅复制 Skill 文件，不复制 `trustee-data/`。

第一轮可使用：

> 使用 $medical-assistant，为我整理 2025、2026 年体检和相关门诊资料，准备下次
> 就诊时向医生确认的问题。请自己决定怎样比较资料，必要时编写代码。说明哪些变化
> 有依据、哪些不能直接比较，并引用实际读取的报告。不需要先读取 2024 年报告。

随后在同一会话追问：

> 请再查看 2024 年的体检，判断你刚才提到的变化在更早时是否已经出现，并更新摘要。

分析和成果由 Codex 根据真实取回的资料生成。Skill 没有预制结论或统计程序。
可根据实际需要交付 Markdown 报告、图表和问题清单。

## 接入 AgentCore

按 [本地纳管指南](https://help.aliyun.com/zh/agentcore/guidelines-for-the-use-of-local-nanoagent)
创建 Codex 纳管 Worker，并加入个人助理 Team。选择本地模型配置，让 Worker
使用 Guest 中已有的模型接入。安装与连接 Pilot 使用平台当前提供的完整流程。
保留控制台连接命令中 `--` 后的运行时参数，包括 `--plugin-ref` 指定的
TeamHarness 版本；仅安装最新版 Pilot 并省略这些参数，会使用运行时包内的
默认插件版本，可能与云端任务系统不匹配。
项目技能放在指定工作目录的 `.agents/skills/`；若使用用户级技能目录，按官方
指南启用本地资源继承。

Codex 示例把模型凭据保存在 `codex.env`。以 root 安装的 Pilot 使用
`loongsuite-pilot-root.service`，为该服务加载同一个环境文件后再连接 Worker：

```bash
install -d /etc/systemd/system/loongsuite-pilot-root.service.d
cat > /etc/systemd/system/loongsuite-pilot-root.service.d/model-env.conf <<'EOF'
[Service]
EnvironmentFile=/root/.config/confidential-agent/codex/codex.env
EOF
systemctl daemon-reload
systemctl restart loongsuite-pilot-root.service
```

这是部署配置，不修改 Pilot 程序。其他运行账号使用其实际服务名与凭据路径。

给医疗 Worker 的职责描述可以使用：

> 你是个人助理团队的医疗资料助理。用户健康档案保存在 Trustee，使用本机
> medical-assistant Skill，通过 CDH 按需取数；自主阅读、分析和编写代码。
> 为就诊准备有来源的摘要和问题清单，区分病历事实、患者自述和不确定推测。
> 本任务使用合成资料，所有成果标注“合成资料演示”。

给 Leader 的协作说明可以使用：

> 健康档案整理、体检对比和就诊准备任务交给医疗助理。传递用户目标和档案代号，
> 无需让用户将原始报告上传到 Team。按平台现有任务流程跟进和交付成果。

原样 Pilot 的消息和 Trace 内容以实际运行结果为准；此示例不承诺平台只接收
最终摘要。普通推理服务属于本版的信任范围，可能收到医学原文和分析上下文。
通过 Team 提交的最终健康摘要同样属于平台可见数据。

沿用 Codex 示例的 debug 镜像时，部署者可通过 SSH 进入 Guest，仍属于受信任的
管理方。TDX 与 Trustee 的证明不能替代对 Guest 管理权限和运行时安装程序的信任。

临床表述的参考背景：[理解检验结果](https://medlineplus.gov/lab-tests/how-to-understand-your-lab-results/)、
[血糖检查与采样条件](https://medlineplus.gov/lab-tests/blood-glucose-test/)。演示报告内的
数值和参考范围是虚构测试数据，不来自这些资料。

## 停止演示

需要继续演示时保留运行资源；结束后断开对应 Pilot Worker，销毁本次创建的
机密实例和独立 Trustee，并删除不再使用的 demo Team/Workspace。仅删除本次
资源。验证记录、凭据、云资源 ID 和模型生成的报告保存在忽略的 `.local/` 中，
不提交到示例源码。
