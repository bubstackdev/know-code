"""The one environment every script uses to start knowcode: nothing of the user's.

A knowcode process reads its config from KNOWCODE_CONFIG_DIR, else from the user config directory under HOME; that
config's `[workspace] roots` send it indexing the user's repositories. Setting only KNOWCODE_CACHE_DIR, as several
scripts did, still read the real config.toml. `knowcode_env` sets all four (HOME and the three KnowCode directories,
each under one scratch base) and turns the update check off, and refuses to hand back the real home or the real
config directory, whatever the caller overrides.
"""

import os
import pwd
from pathlib import Path

APP = "dev.bubstack.knowcode"


def real_home() -> Path:
    """The account's home from the user database, not from HOME (which a caller may already have replaced)."""
    return Path(pwd.getpwuid(os.getuid()).pw_dir).resolve()


def real_config_dirs() -> list[Path]:
    """Where a knowcode run without KNOWCODE_CONFIG_DIR would read config.toml: macOS's, then the XDG default."""
    home = real_home()
    return [home / "Library" / "Application Support" / APP, home / ".config" / APP]


def knowcode_env(base, start=None, *, cache=None, config=None, data=None, **extra) -> dict:
    """The environment for knowcode runs under `base`: HOME, KNOWCODE_CACHE_DIR, KNOWCODE_CONFIG_DIR and
    KNOWCODE_DATA_DIR are base/home, base/cache, base/config and base/data (created) unless `cache`, `config` or `data`
    name another directory, and KNOWCODE_UPDATE_CHECK is 0. `start` is the environment to build on (os.environ by
    default); `extra` adds or replaces variables. Raises ValueError if the result would use the real home or config."""
    base = Path(base)
    dirs = {
        "HOME": base / "home",
        "KNOWCODE_CACHE_DIR": Path(cache) if cache is not None else base / "cache",
        "KNOWCODE_CONFIG_DIR": Path(config) if config is not None else base / "config",
        "KNOWCODE_DATA_DIR": Path(data) if data is not None else base / "data",
    }
    env = dict(os.environ if start is None else start)
    env.update({k: str(v) for k, v in dirs.items()}, KNOWCODE_UPDATE_CHECK="0")
    env.update(extra)
    # Before creating anything: a refused directory (the real config directory) must not be made either.
    check(env)
    for key in ("HOME", "KNOWCODE_CACHE_DIR", "KNOWCODE_CONFIG_DIR", "KNOWCODE_DATA_DIR"):
        Path(env[key]).mkdir(parents=True, exist_ok=True)
    return env


def check(env) -> None:
    """Raises ValueError if `env` would let knowcode use the account's real home or config directory."""
    home = real_home()
    if Path(env.get("HOME", "")).resolve() == home:
        raise ValueError(f"HOME is the real home {home}")
    config = Path(env.get("KNOWCODE_CONFIG_DIR", "")).resolve() if env.get("KNOWCODE_CONFIG_DIR") else None
    if config is None:
        raise ValueError("KNOWCODE_CONFIG_DIR is unset, so knowcode would read the user config directory")
    for real in real_config_dirs():
        if config == real or real in config.parents:
            raise ValueError(f"KNOWCODE_CONFIG_DIR {config} is the real config directory {real}")
