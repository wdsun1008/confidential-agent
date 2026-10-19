# cagent RPM 安装与 all-in-one 流程

当前支持 Alibaba Cloud Linux 3、x86_64。cagent 使用原生工具，标准
all-in-one 构建和部署不需要宿主机 Docker 或 Podman。Guest 内应用如果启用
PEP，仍可以使用 Guest 自己的容器隔离。

## 构建 RPM

在打包机安装构建依赖，然后从源码根目录执行：

```bash
sudo dnf install -y rpm-build cargo rust gcc glibc-devel pkgconf-pkg-config python3.11 curl tar
packaging/build-rpm.sh
```

产物位于 `target/rpm/RPMS/x86_64/cagent-0.1.0-1.al8.x86_64.rpm`，
SRPM 位于 `target/rpm/SRPMS/`。源码准备阶段缓存并校验固定版本的
cosign、aliyun、ossutil，GitHub 下载默认先尝试国内代理再回退官方地址；
可用 `CA_GITHUB_PROXY_URL` 修改代理。`target/rpm/SOURCES/SHA256SUMS`
记录源码、Cargo vendor 和工具归档的哈希。rpmbuild 内的 Cargo 构建和测试
使用 `--locked --offline`，不会再次下载依赖。

`CAGENT_BUILD_JOBS` 控制打包并行度，默认 4。最终 release 固定为 `1`，
本地验证过程不递增 RPM 的 Release。

## 在干净机器安装

把 cagent 和随项目发布的 Shelter RPM 放在同一目录，安装：

```bash
sudo dnf install -y ./shelter-0.3.2-1.al8.x86_64.rpm ./cagent-0.1.0-1.al8.x86_64.rpm
cagent version
rpm -q cagent shelter attestation-challenge-client libtdx-verify trusted-network-gateway
```

RPM 安装 CLI、Guest 构建资产和示例，并自动解析原生验证工具的依赖。
不需要在这台机器安装 Rust/Cargo、克隆源码或编译 CA。
目前 Shelter RPM 仍携带构建依赖，因此这也是可用于构建镜像的安装环境。

将模板复制到可写工作目录，保留示例间的相对引用：

```bash
mkdir -p ~/cagent-work
cp -a /usr/share/cagent/examples /usr/share/cagent/common /usr/share/cagent/software-versions.json ~/cagent-work/
cd ~/cagent-work
export PATH=/usr/libexec/cagent/bin:$PATH
aliyun configure
```

CA 的发布和云部署会读取宿主机的 AK/StsToken profile；也可使用
AccessKey 环境变量。凭证优先级和命名 profile 见
[all-in-one 文档](../examples/allinone/README.md#build-and-publish)。

## 构建和发布

按目标地域检查 `examples/allinone/build.yaml`，然后执行：

```bash
cagent build --spec examples/allinone/build.yaml
cagent image publish allinone-builder --spec examples/allinone/build.yaml
openssl genpkey -algorithm ED25519 -out publisher.key
openssl pkey -in publisher.key -pubout -out publisher.pem
cagent image export allinone-builder --account-id ACCOUNT_ID \
  --signing-key publisher.key --output image-release.json
```

记录 export 输出的完整 manifest digest。镜像上传的
`CA_IMAGE_OSS_ENDPOINT=auto` 会检测同地域 ECS 内网端点，否则使用公网。
发布不重新构建镜像。

## 部署和对话

复制并修改 `examples/allinone/instance.yaml`：使用新的 service ID，
填写 release、公钥、manifest digest、账号和实例规格。按照
[harness 原生配置说明](../examples/allinone/README.md#image-configuration)
准备 `runtime.json` 与该 harness 的模型凭证资源。例如选择 OpenClaw
gateway 时，同时声明 18789 应用端口；选择 CLI harness 时保留空端口列表。

```bash
cagent peering add --role operator --cidr DEPLOYER_PUBLIC_IP/32 --label deployer
cagent spec validate --spec instance.yaml
cagent deploy --spec instance.yaml
cagent status --live
cagent report --json
cagent ssh SERVICE -- cai-agent --check
cagent ssh SERVICE --tty -- cai-agent
```

最后一条命令进入所选 harness 的交互入口。配置模型后发送实际问题并确认
模型返回内容；`active` 或端口就绪只说明部署状态，不等同于模型对话成功。
Gateway 的 HTTP/WebSocket 客户端使用原生 TNG 隧道：

```bash
cagent connect start --service SERVICE --ready-json connect.json --log-file connect.log
cagent connect stop --ready-json connect.json
cagent destroy SERVICE
```

后台连接记录使用 v2 格式，包含 PID、进程启动时刻、系统 boot ID 和日志
路径，停止时校验进程身份。旧 tools 容器产生的 v1 ready 文件不用于管理
原生隧道，升级前应先停止旧连接。

销毁消费者不会删除共享云镜像。发布者确认无消费者依赖后，使用
`image unpublish` 退役镜像；导出过的镜像需要显式 `--force`。
