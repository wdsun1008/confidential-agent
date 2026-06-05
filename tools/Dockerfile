ARG BASE_IMAGE=alibaba-cloud-linux-3-registry.cn-hangzhou.cr.aliyuncs.com/alinux3/alinux3:latest

FROM ${BASE_IMAGE} AS tools

ARG INSTALL_PACKAGES="binutils ca-certificates curl rpm tar"
ARG COSIGN_VERSION=3.0.6
ARG REKOR_VERSION=1.5.1
ARG ALIYUN_CLI_VERSION=3.0.232
ARG OSSUTIL_VERSION=1.7.18
ARG COSIGN_SHA256=c956e5dfcac53d52bcf058360d579472f0c1d2d9b69f55209e256fe7783f4c74
ARG REKOR_CLI_SHA256=0b4964af85477892c37039fb80793b151864970d19838873eaa1a777ca2fb813
ARG ALIYUN_CLI_SHA256=f6dca50b17e745881c5bb5d2e02ea7382bf1cf2a6d3ec76488b071a04a06c0e4
ARG OSSUTIL_SHA256=c0d1c40cb406ae2295a4d1e6a8a0aef8b4319d609c6b8acd742348c88734b697

RUN yum install -y \
        --setopt=install_weak_deps=False \
        ${INSTALL_PACKAGES} && \
    yum clean all && \
    rm -rf /var/cache/dnf /var/cache/yum

COPY hack/tng-2.6.0 /usr/bin/tng

RUN curl -fsSL -o /usr/bin/cosign "https://github.com/sigstore/cosign/releases/download/v${COSIGN_VERSION}/cosign-linux-amd64" && \
    echo "${COSIGN_SHA256}  /usr/bin/cosign" | sha256sum -c - && \
    curl -fsSL -o /usr/bin/rekor-cli "https://github.com/sigstore/rekor/releases/download/v${REKOR_VERSION}/rekor-cli-linux-amd64" && \
    echo "${REKOR_CLI_SHA256}  /usr/bin/rekor-cli" | sha256sum -c - && \
    chmod 0755 /usr/bin/cosign /usr/bin/rekor-cli

RUN curl -fsSL -o /tmp/aliyun-cli.tgz "https://aliyuncli.alicdn.com/aliyun-cli-linux-${ALIYUN_CLI_VERSION}-amd64.tgz" && \
    echo "${ALIYUN_CLI_SHA256}  /tmp/aliyun-cli.tgz" | sha256sum -c - && \
    tar xzf /tmp/aliyun-cli.tgz -C /usr/bin/ && \
    rm -f /tmp/aliyun-cli.tgz && \
    chmod 0755 /usr/bin/aliyun

RUN curl -fsSL -o /usr/bin/ossutil64 "https://gosspublic.alicdn.com/ossutil/${OSSUTIL_VERSION}/ossutil64" && \
    echo "${OSSUTIL_SHA256}  /usr/bin/ossutil64" | sha256sum -c - && \
    chmod 0755 /usr/bin/ossutil64

RUN yum install -y \
        --setopt=install_weak_deps=False \
        libtdx-verify \
        attestation-challenge-client && \
    yum clean all && \
    rm -rf /var/cache/dnf /var/cache/yum && \
    ! rpm -q trustee && \
    strip --strip-unneeded /usr/bin/attestation-challenge-client /usr/bin/cosign /usr/bin/rekor-cli /usr/bin/aliyun /usr/bin/ossutil64

FROM ${BASE_IMAGE}

COPY --from=tools /usr/bin/attestation-challenge-client /usr/bin/attestation-challenge-client
COPY --from=tools /usr/bin/tng /usr/bin/tng
COPY --from=tools /usr/bin/cosign /usr/bin/cosign
COPY --from=tools /usr/bin/rekor-cli /usr/bin/rekor-cli
COPY --from=tools /usr/bin/aliyun /usr/bin/aliyun
COPY --from=tools /usr/bin/ossutil64 /usr/bin/ossutil64
COPY --from=tools /etc/sgx_default_qcnl.conf /etc/sgx_default_qcnl.conf
COPY hack/tng-2.6.0 /opt/confidential-agent/hack/tng-2.6.0
COPY --from=tools /usr/lib64/libsgx_dcap_quoteverify.so.1.22.100.3 /usr/lib64/libsgx_dcap_quoteverify.so.1.22.100.3
COPY --from=tools /usr/lib64/libsgx_default_qcnl_wrapper.so.1.22.100.3 /usr/lib64/libsgx_default_qcnl_wrapper.so.1.22.100.3
COPY --from=tools /usr/lib64/libdcap_quoteprov.so.1.13.109.3 /usr/lib64/libdcap_quoteprov.so.1.13.109.3
COPY --from=tools /usr/share/sgx/tee_appraisal_policy.wasm /usr/share/sgx/tee_appraisal_policy.wasm
COPY tools/policies /opt/confidential-agent/policies
# This is the attestation-challenge-client default OPA policy location; it does not install the trustee RPM.
COPY tools/policies/trustee-opa-default.rego /var/lib/attestation/token/ear/policies/opa/default.rego

RUN ln -sf libsgx_dcap_quoteverify.so.1.22.100.3 /usr/lib64/libsgx_dcap_quoteverify.so.1 && \
    ln -sf libsgx_default_qcnl_wrapper.so.1.22.100.3 /usr/lib64/libsgx_default_qcnl_wrapper.so.1 && \
    ln -sf libdcap_quoteprov.so.1.13.109.3 /usr/lib64/libdcap_quoteprov.so.1 && \
    chmod 0755 /usr/bin/attestation-challenge-client /usr/bin/tng /usr/bin/cosign /usr/bin/rekor-cli /usr/bin/aliyun /usr/bin/ossutil64 /opt/confidential-agent/hack/tng-2.6.0 && \
    chmod 0644 /etc/sgx_default_qcnl.conf /usr/share/sgx/tee_appraisal_policy.wasm /var/lib/attestation/token/ear/policies/opa/default.rego /opt/confidential-agent/policies/*.rego && \
    chmod 0755 /usr/share/sgx /var/lib/attestation /var/lib/attestation/token /var/lib/attestation/token/ear /var/lib/attestation/token/ear/policies /var/lib/attestation/token/ear/policies/opa /opt/confidential-agent /opt/confidential-agent/policies /opt/confidential-agent/hack

RUN /usr/bin/tng --version | grep -q '^tng 2\.6\.0' && \
    tng_sum="$(sha256sum /usr/bin/tng)" && \
    hack_tng_sum="$(sha256sum /opt/confidential-agent/hack/tng-2.6.0)" && \
    test "${tng_sum%% *}" = "${hack_tng_sum%% *}" && \
    /usr/bin/cosign version >/dev/null && \
    /usr/bin/rekor-cli version >/dev/null && \
    /usr/bin/aliyun version >/dev/null && \
    /usr/bin/ossutil64 --version >/dev/null && \
    /opt/confidential-agent/hack/tng-2.6.0 --version | grep -q '^tng 2\.6\.0' && \
    /usr/bin/attestation-challenge-client --help >/dev/null
