#!/bin/bash
# build + deploy d3d12_xs, run the game under mdbg, summarize.
cd "D:/projects/TomBox"
SB="C:/Users/drych/AppData/Local/Temp/claude/71a104f8182872d2/161832fd-3a57-4517-85d3-2c387a0e8b8e/scratchpad"
export RUSTFLAGS="-C target-feature=+crt-static"
cargo build --release -p d3d12_xs -p tombox-xshader 2>&1 | grep -E "^error" -A12 | head -30
cp target/release/d3d12_xs.dll "input/extracted/Mount/FortniteGame/Binaries/XSX/d3d12_xs.dll" || echo LOCKED
rm -rf /c/Users/drych/AppData/Local/Temp/tombox_shaders
export TOMBOX_LOG="C:/Users/drych/AppData/Local/Temp/tombox_g.log"; rm -f "$TOMBOX_LOG"
timeout ${1:-240} "$SB/mdbg.exe" "G:/FortniteGame/Binaries/XSX/FortniteClient-XSX-Shipping.exe" -unattended > "$SB/run.txt" 2>&1
grep -aE "shader_xlat FAIL|PSO_X (.*FAIL)|PSO_X untrans" "$TOMBOX_LOG" | cut -c15-240 | tail -5
grep -aE "PSO_X counts" "$TOMBOX_LOG" | tail -1 | cut -c15-200; grep -a "PresentX frame" "$TOMBOX_LOG" | tail -1 | cut -c15-80
echo "=== exit ==="; grep -aE "EXIT_PROCESS|\[EXC\] (AV|code=0x0000087)|Fatal error|Line: |time limit" "$SB/run.txt" | tail -3
grep -a -A10 "^\[STACK\]" "$SB/run.txt" | grep -v cfgmgr | head -6
grep -a "\[ODS\]" "$SB/run.txt" | grep -avE "Setting CVar|LogConfig|D3D12 WARN|Runnable|Malloc Size" | tail -${2:-5} | cut -c1-230
ls /c/Users/drych/AppData/Local/Temp/tombox_shaders 2>/dev/null | head
