#!/usr/bin/env python3
"""Exercise the real Rust/C++ boundary on Linux; no Windows/audio installation."""
import os, shutil, subprocess, sys, tempfile
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]

def main():
    if sys.platform!='linux':raise SystemExit('This stand-in WDK host harness currently runs on Linux.')
    compiler=shutil.which('c++')
    if not compiler:raise SystemExit('A C++17 host compiler is required.')
    with tempfile.TemporaryDirectory(prefix='babel-windows-shim-') as temp:
        target=Path(temp)/'rust'
        environment={**os.environ,'CARGO_INCREMENTAL':'0'}
        subprocess.run(['cargo','build','--locked','--manifest-path',str(ROOT/'transport/Cargo.toml'),
                        '--target-dir',str(target),'--release','--features','kernel'],check=True,env=environment)
        output=Path(temp)/'shim-test'
        subprocess.run([compiler,'-std=c++17','-pthread','-Wno-unknown-pragmas','-I',str(ROOT/'tests/wdk_stub'),
                        '-I',str(ROOT/'shim'),str(ROOT/'tests/shim_host.cpp'),str(ROOT/'shim/BabelTransport.cpp'),
                        str(target/'release/libbabel_windows_transport.a'),'-o',str(output)],check=True)
        subprocess.run([str(output)],check=True,timeout=30)
    print('Rust + C++ ABI shim host tests passed (not a WDK/Windows validation).')

if __name__=='__main__':main()
