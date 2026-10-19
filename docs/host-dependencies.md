# 构建机、部署机与 tools 容器的依赖

本清单按当前调用路径区分依赖。仅部署指使用已发布镜像和签名 release，
不包含本地 QCOW2 上传、云镜像导入或应用构建。

## 宿主机

| 组件 | 构建机 | 仅部署已有镜像 |
|---|---|---|
| CA CLI | 必需 | 必需；可直接分发预编译二进制 |
| Shelter、Terraform、固定版本 providers | 必需 | 必需，负责 release 验证和云资源生命周期；目前在宿主机，不在 tools 容器 |
| Docker 命令接口或兼容的 Podman | tools 执行与 guest 资产提取 | challenge 注入、证明报告、TNG connect 等路径使用 |
| mkosi、Python、QEMU、LVM、cryptsetup、文件系统/分区/签名工具 | 镜像构建需要 | 已发布镜像部署路径不构建磁盘；当前完整 Shelter RPM 仍强制安装这批依赖 |
| Rust/Cargo、gcc、开发库 | 从源码构建 CA 时需要 | 预编译 CA 不需要 |
| Node/npm、Python 应用环境、CUDA、vLLM 等 | 按 AppSpec 在 guest rootfs 内安装；部分 one-click 流程另装宿主机 OpenClaw | 应用环境已经在云镜像内，部署宿主机不需要 |
| OpenSSH 客户端 | 按使用场景 | `ca ssh` 需要 `ssh`；CA 自己生成实例密钥，不调用 `ssh-keygen` |
| 宿主机 aliyun、jq、bash | 按使用场景 | 当前 ALB zone 查询会直接调用宿主机 `aliyun`、`jq`、`bash`，未被 tools 容器覆盖 |
| 凭证、CA 证书、私有可写 state、网络访问 | 必需 | 必需 |

注意：现有 `one-click/install.sh` 的 install/deploy 流程仍安装构建依赖、
编译 host/guest 二进制并构建 tools 镜像。它不是部署已有镜像的轻量安装器。
`shelter-slim` 也仍包含镜像构建功能，不能当作已完成拆分的部署包。

## tools 镜像实际提供的工具

| 工具 | 用途 | 仅部署已有镜像是否需要 |
|---|---|---|
| attestation-challenge-client | 设置参考值、远程证明、加密资源注入、证明报告 | challenge 模式需要；Trustee 的资源交付走另一条路径 |
| tng | 客户端可信隧道 | `ca connect` 需要 |
| cosign | 构建参考值签名、keyless AgentCard 签名；也注入 guest | 部署时启用 AgentCard 签名才需要；release 验签由 Shelter 原生实现 |
| rekor-cli | 构建参考值上传 Rekor | 单纯消费已有镜像不需要重新上传 |
| aliyun | CA 的镜像发布/清理等云 API 调用 | 通常不是基本部署需要；ALB 的宿主机调用仍需另行处理 |
| ossutil64 | 镜像上传、发布对象管理 | 已发布镜像消费路径跳过上传和导入 |

镜像还携带 SGX/DCAP quote verification/provider 库、QCNL 配置、appraisal
WASM 和 OPA 策略。实测 attestation client 还依赖 `libtss2-mu`、
`libstdc++`、glibc 等；TNG 也有 glibc/libgcc 等动态依赖。
四个 Go 工具是独立二进制，主要的系统库兼容负担集中在 attestation client。

2026-10-19 在指定构建机上只读检查：tools 镜像本地大小为
509,531,641 字节（约 486 MiB，非压缩下载量）。6 个工具本身共
277,281,192 字节（约 264 MiB）；其中 attestation client 约 24 MiB，
TNG 约 54 MiB。宿主机已装 aliyun，但没有其余 5 个原生工具。
这些数据不是部署安装耗时或冷启动对比测试。

## 必要性评估与建议

部署机上的容器有实际的版本和动态库隔离价值，尤其是 DCAP/TPM 依赖。
代价是容器运行时、镜像分发，以及凭证、文件挂载、代理和后台隧道生命周期
的额外衔接。当前使用 host network，并挂载工作目录/state；不能把它视为
严格的凭证隔离边界。

建议先实现部署专用安装路径，再决定是否移除默认工具容器：

1. 将 Shelter 的运行时部署包与磁盘构建依赖分开，部署机只装 CA、
   Shelter、Terraform/providers 和所选功能的客户端依赖。
2. 分发预编译 CA 和预构建 tools 镜像，部署机器不运行 Cargo、
   Dockerfile 或 guest 构建脚本；解决 ALB 对宿主机 aliyun/jq 的遗漏。
3. 对比该部署环境与原生工具包：原生包需自动安装并固定 attestation
   client、TNG、DCAP/TPM 库和策略，不能让用户手工补依赖。
   固定版本 AliLinux 3 上原生包是可行的候选；跨发行版时容器更容易统一运行环境。
4. 在干净机器验证完整的 deploy、注入/Trustee、report、connect、ssh、
   destroy，比较安装包体积、冷安装耗时和宿主机包数。
   只有验证通过，才决定容器是否默认保留；目前尚未做这组对比。

移除容器还需替换 guest 资产提取、Sigstore wrapper、证明执行器和
connect 后台容器的启动/状态/退出处理，不能只删除 Dockerfile。

源码依据：`one-click/lib/deps-alinux.sh`、`one-click/lib/deploy.sh`、
`tools/Dockerfile`、`cli/src/app/{commands,tools,images,guest_assets,report}.rs`，
以及 Shelter 的 `shelter.spec` 和 Terraform ALB renderer。
