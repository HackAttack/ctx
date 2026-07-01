"""SDK configuration objects."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Mapping, Optional


@dataclass(frozen=True)
class LocalConfig:
    """Configuration for the local CLI adapter."""

    ctx_binary: str = "ctx"
    data_root: Optional[Path] = None
    env: Optional[Mapping[str, str]] = None
    cwd: Optional[Path] = None
    timeout: Optional[float] = None


@dataclass(frozen=True)
class HostedConfig:
    """Placeholder configuration for a future hosted agent-history-v1 transport."""

    base_url: str
    api_key: Optional[str] = None
    timeout: Optional[float] = None
