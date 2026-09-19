#!/usr/bin/env python3
"""Generate the vendored provider-catalog snapshot from a models.dev api.json.

Usage: python scripts/generate_catalog.py <models.dev api.json> <output snapshot.json>

The snapshot keeps only providers MCode can serve with its three first-party
wire protocols (anthropic-messages, openai-completions) and normalizes every
entry to the compact camelCase schema the mcode-catalog crate embeds.
"""
import json
import sys
from datetime import date, datetime, timezone

MAX_STRING_BYTES = 8 * 1024
MAX_MODELS_PER_PROVIDER = 512

# Known base URLs for first-party labs that models.dev lists without an `api`
# field because the AI SDK package embeds the endpoint.
ENDPOINT_FIXES = {
    "anthropic": ("anthropic-messages", "https://api.anthropic.com"),
    "openai": ("openai-completions", "https://api.openai.com/v1"),
    "groq": ("openai-completions", "https://api.groq.com/openai/v1"),
    "mistral": ("openai-completions", "https://api.mistral.ai/v1"),
    "xai": ("openai-completions", "https://api.x.ai/v1"),
    "cerebras": ("openai-completions", "https://api.cerebras.ai/v1"),
    "perplexity": ("openai-completions", "https://api.perplexity.ai"),
}

# Providers excluded from presets: cloud consoles with non-portable auth
# (cloud SDKs, OAuth device flows) rather than a plain API key.
EXCLUDED_PROVIDERS = {"github-copilot"}


def resolve_wire(provider_id: str, raw: dict) -> tuple[str, str] | None:
    """Returns (wire kind, base URL) for one models.dev provider entry."""
    if provider_id in EXCLUDED_PROVIDERS:
        return None
    npm = clean_text(raw.get("npm"))
    api = clean_text(raw.get("api"))
    if "anthropic" in npm:
        kind = "anthropic-messages"
    elif any(token in npm for token in ("bedrock", "vertex", "azure", "google")):
        return None
    else:
        kind = "openai-completions"
    if api.startswith("https://"):
        return (kind, api)
    fix = ENDPOINT_FIXES.get(provider_id)
    if fix is not None:
        return fix
    return None


def clean_text(value, fallback=""):
    if not isinstance(value, str):
        return fallback
    value = value.strip()
    if len(value.encode("utf-8")) > MAX_STRING_BYTES:
        return fallback
    return value


def build_model(model_id: str, raw: dict) -> dict | None:
    if not isinstance(raw, dict):
        return None
    if isinstance(raw.get("status"), str) and raw["status"].strip():
        # Deprecated or retired entries are not offered as presets.
        return None
    name = clean_text(raw.get("name"), model_id) or model_id
    limit = raw.get("limit") if isinstance(raw.get("limit"), dict) else {}
    cost = raw.get("cost") if isinstance(raw.get("cost"), dict) else {}

    def number(value):
        return value if isinstance(value, (int, float)) and value >= 0 else None

    model = {
        "id": model_id,
        "name": name,
        "reasoning": bool(raw.get("reasoning")),
        "toolCall": bool(raw.get("tool_call")),
        "attachment": bool(raw.get("attachment")),
        "context": int(number(limit.get("context")) or 0),
        "output": int(number(limit.get("output")) or 0),
    }
    cost_in = number(cost.get("input"))
    cost_out = number(cost.get("output"))
    if cost_in is not None or cost_out is not None:
        model["costIn"] = cost_in
        model["costOut"] = cost_out
    return model


def build_provider(provider_id: str, raw: dict) -> dict | None:
    if not isinstance(raw, dict):
        return None
    wire = resolve_wire(provider_id, raw)
    if wire is None:
        return None
    kind, base_url = wire
    models = []
    seen = set()
    raw_models = raw.get("models") if isinstance(raw.get("models"), dict) else {}
    for model_id, model in raw_models.items():
        if len(models) >= MAX_MODELS_PER_PROVIDER:
            break
        if not isinstance(model_id, str) or not model_id or model_id in seen:
            continue
        built = build_model(model_id, model)
        if built is not None:
            seen.add(model_id)
            models.append(built)
    if not models:
        return None
    return {
        "id": provider_id,
        "name": clean_text(raw.get("name"), provider_id) or provider_id,
        "kind": kind,
        "baseUrl": base_url,
        "doc": clean_text(raw.get("doc")) or None,
        "models": models,
    }


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: generate_catalog.py <api.json> <snapshot.json>", file=sys.stderr)
        return 2
    with open(sys.argv[1], "r", encoding="utf-8") as handle:
        source = json.load(handle)

    providers = []
    for provider_id, raw in source.items():
        built = build_provider(provider_id, raw)
        if built is not None:
            providers.append(built)
    providers.sort(key=lambda provider: provider["id"])

    snapshot = {
        "formatVersion": 1,
        "kind": "mcode-catalog-snapshot",
        "source": "models.dev",
        "generatedAt": datetime.now(timezone.utc).strftime("%Y-%m-%d"),
        "providers": providers,
    }
    payload = json.dumps(snapshot, ensure_ascii=False, separators=(",", ":"))
    with open(sys.argv[2], "w", encoding="utf-8", newline="\n") as handle:
        handle.write(payload)
        handle.write("\n")

    total_models = sum(len(provider["models"]) for provider in providers)
    anthropic = sum(1 for p in providers if p["kind"] == "anthropic-messages")
    print(
        f"{len(providers)} providers ({anthropic} anthropic-messages), "
        f"{total_models} models, {len(payload) / 1024:.0f} KiB"
    )
    for provider in providers[:8]:
        print(f"  {provider['id']}: {len(provider['models'])} models ({provider['kind']})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
