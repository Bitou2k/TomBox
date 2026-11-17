TomBox compat build 2 (additive) - 2026-11-16
Add these TWO DLLs ALONGSIDE the four from build 1 (keep all 6 together in the
FortniteClient-XSX-Shipping.exe folder):
  dxcompiler_xs.dll  - DXC shader compiler factory (DxcCreateInstance) - logging stub
  AcpHal.dll         - Xbox audio HAL (6 funcs) - logging stubs
Both return E_NOTIMPL and log to %TEMP%\tombox_compat.log (or %TOMBOX_LOG%).
After adding these, relaunch and tell me:
  - does it start now, or is ANOTHER dll requested? (give the exact name)
  - send the tombox_compat.log - the call order tells us what to build next.
Likely-next candidates if more are requested: GameInput.dll, XAudio2_9.dll, or the
ext-ms-win-rtcore-ntuser-* API sets. We'll handle each as it appears.
