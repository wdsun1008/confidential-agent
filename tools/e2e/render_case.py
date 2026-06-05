#!/usr/bin/env python3.11
"""Render E2E case fixtures from checked-in templates."""

import argparse
import base64
import json
import os
import re
import shutil
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CASES = ROOT / "tools" / "e2e" / "cases"


def yaml_quote(value: str) -> str:
    if "\n" in value or "\r" in value:
        raise SystemExit("YAML scalar values must not contain newlines")
    return "'" + value.replace("'", "''") + "'"


def render_rekor_block(context):
    if context["REFERENCE_VALUES"] != "rekor":
        return ""
    cosign_key = context.get("COSIGN_KEY", "")
    if not cosign_key:
        raise SystemExit("COSIGN_KEY is required when REFERENCE_VALUES=rekor")
    return "\n".join(
        [
            "  rekor:",
            f"    cosign_key: {yaml_quote(cosign_key)}",
            f"    slsa_generator: {yaml_quote(context['SLSA_GENERATOR'])}",
            "    required: true",
        ]
    )


def render_base_image_line(context):
    if context["BUILD_BACKEND"] == "base-image":
        return f"  base_image: {yaml_quote(context['BASE_IMAGE'])}"
    return ""


def decode_jwt_payload(token: str) -> dict:
    parts = token.split(".")
    if len(parts) < 2:
        raise SystemExit("CA_A2A_SIGSTORE_IDENTITY_TOKEN must be a JWT when used to derive signer pins")
    payload = parts[1]
    payload += "=" * (-len(payload) % 4)
    try:
        raw = base64.urlsafe_b64decode(payload.encode("ascii"))
        return json.loads(raw.decode("utf-8"))
    except Exception as exc:
        raise SystemExit(f"failed to decode CA_A2A_SIGSTORE_IDENTITY_TOKEN JWT payload: {exc}") from exc


def apply_a2a_signer_defaults(context):
    if context.get("E2E_A2A_SIGNER_ISSUER") and not context.get("A2A_SIGNER_ISSUER"):
        context["A2A_SIGNER_ISSUER"] = context["E2E_A2A_SIGNER_ISSUER"]
    if context.get("E2E_A2A_SIGNER_SUBJECT") and not context.get("A2A_SIGNER_SUBJECT"):
        context["A2A_SIGNER_SUBJECT"] = context["E2E_A2A_SIGNER_SUBJECT"]

    token = context.get("CA_A2A_SIGSTORE_IDENTITY_TOKEN", "").strip()
    if token and (not context.get("A2A_SIGNER_ISSUER") or not context.get("A2A_SIGNER_SUBJECT")):
        payload = decode_jwt_payload(token)
        if not context.get("A2A_SIGNER_ISSUER"):
            context["A2A_SIGNER_ISSUER"] = str(payload.get("iss") or "")
        if not context.get("A2A_SIGNER_SUBJECT"):
            context["A2A_SIGNER_SUBJECT"] = str(payload.get("sub") or "")

    context.setdefault("A2A_SIGNER_ISSUER", "")
    context.setdefault("A2A_SIGNER_SUBJECT", "")


def a2a_signing_enabled(context):
    raw = str(context.get("E2E_A2A_SIGNING") or context.get("A2A_SIGNING") or "").strip().lower()
    return raw in {"1", "true", "yes", "y", "on", "signed", "sigstore", "keyless"}


def render_a2a_signing_block(context):
    if not a2a_signing_enabled(context):
        return ""
    apply_a2a_signer_defaults(context)
    issuer = context.get("A2A_SIGNER_ISSUER", "").strip()
    subject = context.get("A2A_SIGNER_SUBJECT", "").strip()
    if not issuer or not subject:
        raise SystemExit(
            "E2E_A2A_SIGNING=1 requires A2A_SIGNER_ISSUER/A2A_SIGNER_SUBJECT, "
            "E2E_A2A_SIGNER_*, or a JWT CA_A2A_SIGSTORE_IDENTITY_TOKEN with iss/sub claims"
        )
    return "\n".join(
        [
            "  signing:",
            "    mode: sigstore-keyless",
            "    required: true",
            f"    expected_issuer: {yaml_quote(issuer)}",
            f"    expected_subject: {yaml_quote(subject)}",
        ]
    )


def context_from_env(work_dir):
    context = {key: value for key, value in os.environ.items()}
    def default(name, value):
        if not context.get(name):
            context[name] = value

    def default_zone_id(region):
        if region == "cn-hongkong":
            return "cn-hongkong-d"
        return "cn-beijing-i"

    def default_instance_type(region):
        if region == "cn-hongkong":
            return "ecs.g8i.xlarge"
        if region == "cn-beijing":
            return "ecs.g9i.xlarge"
        return "ecs.g8i.xlarge"

    default("ROOT_DIR", str(ROOT))
    context["WORK_DIR"] = str(work_dir)
    default("BUILD_BACKEND", "mkosi")
    default("BASE_IMAGE", "/root/images/alinux3.qcow2")
    default("REFERENCE_VALUES", "rekor")
    default("SLSA_GENERATOR", "/usr/libexec/shelter/slsa/slsa-generator")
    default("DASHSCOPE_BASE_URL", "https://dashscope.aliyuncs.com/compatible-mode/v1")
    default("DASHSCOPE_MODEL", "qwen3.7-max")
    default("REGION", "cn-beijing")
    default("ZONE_ID", default_zone_id(context["REGION"]))
    default("INSTANCE_TYPE", default_instance_type(context["REGION"]))
    default("MCP_SERVICE_ID", "mcp")
    default("DISK_GB", "200")
    default("CAI_PEP", str(ROOT / "target" / "debug" / "cai-pep"))
    context["BASE_IMAGE_LINE"] = render_base_image_line(context)
    context["REKOR_BLOCK"] = render_rekor_block(context)
    context["DASHSCOPE_KEY"] = (
        context.get("DASHSCOPE_KEY")
        or context.get("DASHSCOPE_API_KEY")
        or context.get("BAILIAN_API_KEY", "")
    )
    context["A2A_SIGNING_BLOCK"] = ""
    return context


TOKEN_RE = re.compile(r"\{\{(raw|yaml|json):([A-Z0-9_]+)\}\}")


def render_template(text, context):
    def replace(match):
        kind, name = match.groups()
        value = context.get(name, "")
        if kind == "raw":
            return value
        if kind == "yaml":
            return yaml_quote(value)
        if kind == "json":
            return json.dumps(value, ensure_ascii=False)
        raise AssertionError(kind)

    return TOKEN_RE.sub(replace, text)


def copy_asset(root, work_dir, entry):
    source = (root / entry["source"]).resolve()
    target = work_dir / entry["target"]
    if target.exists():
        if target.is_dir():
            shutil.rmtree(target)
        else:
            target.unlink()
    target.parent.mkdir(parents=True, exist_ok=True)
    if source.is_dir():
        shutil.copytree(source, target)
    else:
        shutil.copy2(source, target)


def render(case, work_dir):
    case_dir = CASES / case
    config_path = case_dir / "case.json"
    if not config_path.exists():
        raise SystemExit(f"unknown E2E case: {case}")
    config = json.loads(config_path.read_text(encoding="utf-8"))
    context = context_from_env(work_dir)
    context.update({key: str(value) for key, value in config.get("context", {}).items()})
    context["A2A_SIGNING_BLOCK"] = render_a2a_signing_block(context)

    work_dir.mkdir(parents=True, exist_ok=True)
    for asset in config.get("assets", []):
        copy_asset(ROOT, work_dir, asset)

    for item in config.get("templates", []):
        source = case_dir / item["source"]
        target = work_dir / item["target"]
        target.parent.mkdir(parents=True, exist_ok=True)
        rendered = render_template(source.read_text(encoding="utf-8"), context)
        target.write_text(rendered, encoding="utf-8")
        if item.get("mode"):
            target.chmod(int(item["mode"], 8))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--case", required=True)
    parser.add_argument("--work-dir", required=True, type=Path)
    args = parser.parse_args()
    render(args.case, args.work_dir)


if __name__ == "__main__":
    main()
