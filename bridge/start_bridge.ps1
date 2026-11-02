$ErrorActionPreference='Continue'
$out='F:\__AI__\tmp\arb.log'
$err='F:\__AI__\tmp\arb.err'
$p = Start-Process -FilePath "node" -ArgumentList "F:\__AI__\work\arena_bridge.js" -WindowStyle Hidden -PassThru -RedirectStandardOutput $out -RedirectStandardError $err
Start-Sleep -Seconds 6
if ($p.HasExited) { "EXITED " + $p.ExitCode } else { "RUNNING " + $p.Id }
"---LOG---"
if (Test-Path $out) { Get-Content $out -Tail 10 } else { "no log" }
"---ERR---"
if (Test-Path $err) { Get-Content $err -Tail 10 } else { "no err" }
