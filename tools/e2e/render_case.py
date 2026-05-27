#!/usr/bin/env python3.11
"""Render E2E case fixtures from checked-in templates."""

import argparse
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


def context_from_env(work_dir):
    context = {key: value for key, value in os.environ.items()}
    context.setdefault("ROOT_DIR", str(ROOT))
    context["WORK_DIR"] = str(work_dir)
    context.setdefault("BUILD_BACKEND", "mkosi")
    context.setdefault("BASE_IMAGE", "/root/images/alinux3.qcow2")
    context.setdefault("REFERENCE_VALUES", "rekor")
    context.setdefault("SLSA_GENERATOR", "/usr/libexec/shelter/slsa/slsa-generator")
    context.setdefault("REGION", "cn-beijing")
    context.setdefault("ZONE_ID", "cn-beijing-l")
    context.setdefault("INSTANCE_TYPE", "ecs.g8i.xlarge")
    context.setdefault("DISK_GB", "200")
    context.setdefault("CAI_PEP", str(ROOT / "target" / "debug" / "cai-pep"))
    context["BASE_IMAGE_LINE"] = render_base_image_line(context)
    context["REKOR_BLOCK"] = render_rekor_block(context)
    context["DASHSCOPE_KEY"] = context.get("DASHSCOPE_API_KEY") or context.get("BAILIAN_API_KEY", "")
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
