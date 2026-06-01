ARG BASE_IMAGE=alibaba-cloud-linux-3-registry.cn-hangzhou.cr.aliyuncs.com/alinux3/alinux3:latest

FROM ${BASE_IMAGE} AS tools

ARG INSTALL_PACKAGES="binutils ca-certificates curl rpm"
ARG COSIGN_VERSION=3.0.6
ARG REKOR_VERSION=1.5.1
ARG COSIGN_SHA256=c956e5dfcac53d52bcf058360d579472f0c1d2d9b69f55209e256fe7783f4c74
ARG REKOR_CLI_SHA256=0b4964af85477892c37039fb80793b151864970d19838873eaa1a777ca2fb813

RUN yum install -y \
        --setopt=install_weak_deps=False \
        ${INSTALL_PACKAGES} && \
    yum clean all && \
    rm -rf /var/cache/dnf /var/cache/yum

COPY hack/libtdx-verify-1.22-4.al8.x86_64.rpm /tmp/libtdx-verify.rpm
COPY hack/tng-2.6.0 /usr/bin/tng

RUN curl -fsSL -o /usr/bin/cosign "https://github.com/sigstore/cosign/releases/download/v${COSIGN_VERSION}/cosign-linux-amd64" && \
    echo "${COSIGN_SHA256}  /usr/bin/cosign" | sha256sum -c - && \
    curl -fsSL -o /usr/bin/rekor-cli "https://github.com/sigstore/rekor/releases/download/v${REKOR_VERSION}/rekor-cli-linux-amd64" && \
    echo "${REKOR_CLI_SHA256}  /usr/bin/rekor-cli" | sha256sum -c - && \
    chmod 0755 /usr/bin/cosign /usr/bin/rekor-cli

RUN rpm -Uvh --replacepkgs --nodeps /tmp/libtdx-verify.rpm && \
    rm -f /tmp/libtdx-verify.rpm && \
    yum install -y \
        --setopt=install_weak_deps=False \
        attestation-challenge-client && \
    yum clean all && \
    rm -rf /var/cache/dnf /var/cache/yum && \
    ! rpm -q trustee && \
    strip --strip-unneeded /usr/bin/attestation-challenge-client /usr/bin/tng /usr/bin/cosign /usr/bin/rekor-cli

FROM ${BASE_IMAGE}

COPY --from=tools /usr/bin/attestation-challenge-client /usr/bin/attestation-challenge-client
COPY --from=tools /usr/bin/tng /usr/bin/tng
COPY --from=tools /usr/bin/cosign /usr/bin/cosign
COPY --from=tools /usr/bin/rekor-cli /usr/bin/rekor-cli
COPY --from=tools /etc/sgx_default_qcnl.conf /etc/sgx_default_qcnl.conf
COPY hack/tng-2.6.0 /opt/confidential-agent/hack/tng-2.6.0
COPY hack/libtdx-verify-1.22-4.al8.x86_64.rpm /opt/confidential-agent/hack/libtdx-verify.rpm
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
    chmod 0755 /usr/bin/attestation-challenge-client /usr/bin/tng /usr/bin/cosign /usr/bin/rekor-cli /opt/confidential-agent/hack/tng-2.6.0 && \
    chmod 0644 /etc/sgx_default_qcnl.conf /usr/share/sgx/tee_appraisal_policy.wasm /var/lib/attestation/token/ear/policies/opa/default.rego /opt/confidential-agent/policies/*.rego /opt/confidential-agent/hack/libtdx-verify.rpm && \
    chmod 0755 /usr/share/sgx /var/lib/attestation /var/lib/attestation/token /var/lib/attestation/token/ear /var/lib/attestation/token/ear/policies /var/lib/attestation/token/ear/policies/opa /opt/confidential-agent /opt/confidential-agent/policies /opt/confidential-agent/hack

RUN /usr/bin/tng --version | grep -q '^tng 2\.6\.0' && \
    /usr/bin/cosign version >/dev/null && \
    /usr/bin/rekor-cli version >/dev/null && \
    /opt/confidential-agent/hack/tng-2.6.0 --version | grep -q '^tng 2\.6\.0' && \
    test -s /opt/confidential-agent/hack/libtdx-verify.rpm && \
    /usr/bin/attestation-challenge-client --help >/dev/null
