"""Observed interpreter identity and unforced runtime-mode admission."""
import json
import os
import platform
import sys
import sysconfig

SELECTED_ABIS = ("cp312", "cp314t")


def observe():
    return {
        "implementation": platform.python_implementation(),
        "version": platform.python_version(),
        "executable": sys.executable,
        "cache_tag": sys.implementation.cache_tag,
        "platform": sysconfig.get_platform(),
        "machine": platform.machine(),
        "soabi": sysconfig.get_config_var("SOABI"),
        "ext_suffix": sysconfig.get_config_var("EXT_SUFFIX"),
        "py_gil_disabled": bool(sysconfig.get_config_var("Py_GIL_DISABLED")),
        "gil_enabled": sys._is_gil_enabled() if hasattr(sys, "_is_gil_enabled") else None,
        "gil_env": os.environ.get("PYTHON_GIL"),
        "gil_xoption": sys._xoptions.get("gil"),
    }


def validate(observed, abi):
    if abi not in SELECTED_ABIS:
        raise ValueError("unselected interpreter ABI")
    interpreter = abi.removesuffix("t")
    version = "cp" + "".join(observed["version"].split(".")[:2])
    if observed["implementation"] != "CPython" or version != interpreter or observed["cache_tag"] != "cpython-" + interpreter[2:]:
        raise ValueError("interpreter identity mismatch")
    if observed["gil_env"] is not None or observed["gil_xoption"] is not None:
        raise ValueError("forced GIL mode is not qualification evidence")
    free_threaded = abi.endswith("t")
    if observed["py_gil_disabled"] is not free_threaded:
        raise ValueError("interpreter build ABI mismatch")
    if not observed["soabi"].startswith("cpython-" + abi[2:] + "-") or not observed["ext_suffix"].startswith(".cpython-" + abi[2:] + "-"):
        raise ValueError("interpreter extension ABI mismatch")
    if free_threaded and observed["gil_enabled"] is not False:
        raise ValueError("free-threaded execution requires observed disabled GIL")
    if not free_threaded and observed["gil_enabled"] not in (None, True):
        raise ValueError("ordinary execution has unexpected GIL state")


if __name__ == "__main__":
    print(json.dumps(observe(), sort_keys=True))
