ARG TNG_IMAGE=ghcr.io/inclavare-containers/tng:2.6.0
ARG BASE_IMAGE=alibaba-cloud-linux-3-registry.cn-hangzhou.cr.aliyuncs.com/alinux3/alinux3:latest
ARG ENCLAVE_REPO=https://enclave-cn-beijing.oss-cn-beijing.aliyuncs.com/repo/alinux/3/exp/x86_64/

FROM ${TNG_IMAGE} AS tng

FROM ${BASE_IMAGE} AS tools

ARG ENCLAVE_REPO
ARG INSTALL_PACKAGES="attestation-challenge-client libsgx-dcap-quote-verify libsgx-dcap-default-qpl binutils"

RUN yum install -y \
        --setopt=install_weak_deps=False \
        --repofrompath=enclave-exp,${ENCLAVE_REPO} \
        --setopt=enclave-exp.gpgcheck=0 \
        ${INSTALL_PACKAGES} && \
    yum clean all && \
    rm -rf /var/cache/dnf /var/cache/yum

COPY --from=tng /usr/local/bin/tng /usr/bin/tng

RUN strip --strip-unneeded /usr/bin/attestation-challenge-client /usr/bin/tng

FROM ${BASE_IMAGE}

COPY --from=tools /usr/bin/attestation-challenge-client /usr/bin/attestation-challenge-client
COPY --from=tools /usr/bin/tng /usr/bin/tng
COPY tools/sgx_default_qcnl.conf /etc/sgx_default_qcnl.conf
COPY hack/tng-2.6.0 /opt/confidential-agent/hack/tng-2.6.0
COPY hack/libtdx-verify-1.22-4.al8.x86_64.rpm /opt/confidential-agent/hack/libtdx-verify.rpm
COPY --from=tools /usr/lib64/libsgx_dcap_quoteverify.so.1.13.100.2 /usr/lib64/libsgx_dcap_quoteverify.so.1.13.100.2
COPY --from=tools /usr/lib64/libsgx_default_qcnl_wrapper.so.1.13.107.2 /usr/lib64/libsgx_default_qcnl_wrapper.so.1.13.107.2
COPY --from=tools /usr/lib64/libdcap_quoteprov.so.1.13.107.2 /usr/lib64/libdcap_quoteprov.so.1.13.107.2
COPY --from=tools /usr/share/sgx/tee_appraisal_policy.wasm /usr/share/sgx/tee_appraisal_policy.wasm
COPY tools/policies /opt/confidential-agent/policies
COPY tools/policies/trustee-opa-default.rego /var/lib/attestation/token/ear/policies/opa/default.rego

RUN ln -sf libsgx_dcap_quoteverify.so.1.13.100.2 /usr/lib64/libsgx_dcap_quoteverify.so.1 && \
    ln -sf libsgx_default_qcnl_wrapper.so.1.13.107.2 /usr/lib64/libsgx_default_qcnl_wrapper.so.1 && \
    ln -sf libdcap_quoteprov.so.1.13.107.2 /usr/lib64/libdcap_quoteprov.so.1 && \
    chmod 0755 /usr/bin/attestation-challenge-client /usr/bin/tng /opt/confidential-agent/hack/tng-2.6.0 && \
    chmod 0644 /etc/sgx_default_qcnl.conf /usr/share/sgx/tee_appraisal_policy.wasm /var/lib/attestation/token/ear/policies/opa/default.rego /opt/confidential-agent/policies/*.rego /opt/confidential-agent/hack/libtdx-verify.rpm && \
    chmod 0755 /usr/share/sgx /var/lib/attestation /var/lib/attestation/token /var/lib/attestation/token/ear /var/lib/attestation/token/ear/policies /var/lib/attestation/token/ear/policies/opa /opt/confidential-agent /opt/confidential-agent/policies /opt/confidential-agent/hack

RUN /usr/bin/tng --version | grep -q '^tng 2\.6\.0' && \
    /opt/confidential-agent/hack/tng-2.6.0 --version | grep -q '^tng 2\.6\.0' && \
    test -s /opt/confidential-agent/hack/libtdx-verify.rpm && \
    /usr/bin/attestation-challenge-client --help >/dev/null
