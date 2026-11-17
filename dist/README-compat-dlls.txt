TomBox — Xbox Series compat stub DLLs  (build 1, 2026-11-16)
============================================================

For: Fortnite Xbox Series build 2.0.1461.7919
     FortniteClient-XSX-Shipping.exe

WHAT THESE ARE
--------------
Four stand-in DLLs that export the exact Xbox-only symbols the game imports, so
the Windows loader can resolve its imports and the process can START. They are
NOT full implementations yet:

  xmem.dll      - REAL (partial): XMemAlloc/XMemFree/XMemVirtualAlloc are backed
                  by the real Windows heap/VirtualAlloc. Physical-page APIs are
                  logged stubs.
  d3d12_xs.dll  - logging stubs + the correct IID_ID3D12Device GUID. Device
                  creation returns E_NOTIMPL (no graphics backend yet).
  xg_xs.dll     - XGCreateTextureComputer logging stub (E_NOTIMPL).
  xcrdapi.dll   - DStorageGetFactory logging stub (E_NOTIMPL).

So: the game should now get PAST the "missing DLL" errors and start running its
own code. It will then almost certainly stop/exit when it hits one of the
E_NOTIMPL calls (most likely D3D12 device creation). THAT IS EXPECTED AND USEFUL.

HOW TO USE
----------
1. Put all four .dll files in the SAME folder as FortniteClient-XSX-Shipping.exe
   (…\FortniteGame\Binaries\XSX\).
2. (Optional) set an env var to control the log location:
       set TOMBOX_LOG=C:\tombox_compat.log
   If unset, the log goes to:  %TEMP%\tombox_compat.log
3. Launch the exe.
4. Send me back:
     - the new error/behavior (does it start now? new missing DLL? crash?)
     - the log file (tombox_compat.log) — it lists every stubbed call in order.

WHAT WE LEARN
-------------
- If a DIFFERENT dll is now reported missing -> that's the next one to stub.
- The log's call order tells us exactly which Xbox APIs fire during startup and
  in what sequence, so we know what to implement for real next (the top priority
  will be whatever is called right before it stops).

These DLLs are self-contained (static CRT) and only use standard Windows APIs.
They do nothing but log and provide memory; they make no network calls.
