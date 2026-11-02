param(
  [string]$PromptB64 = '',
  [string]$Session = 'main',
  [string]$Mode = 'agent',
  [string]$Model = '',
  [string]$Fresh = '0',
  [string]$Out = 'F:\__AI__\tmp\last.json',
  [string]$Url = 'http://127.0.0.1:17892/ask'
)
$ErrorActionPreference = 'Stop'
try {
  $p = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($PromptB64))
  $body = @{ prompt = $p; session = $Session; mode = $Mode; stream = $false }
  if ($Model -ne '') { $body.model = $Model }
  if ($Fresh -eq '1') { $body.new = $true }
  $bytes = [Text.Encoding]::UTF8.GetBytes(($body | ConvertTo-Json -Compress))
  $r = Invoke-RestMethod -Uri $Url -Method Post -Body $bytes -ContentType 'application/json; charset=utf-8' -TimeoutSec 1500
  $j = $r | ConvertTo-Json -Compress -Depth 6
  [IO.File]::WriteAllText($Out, $j, [Text.Encoding]::UTF8)
  'OK ' + (Get-Item $Out).Length
} catch {
  $m = $_.Exception.Message
  if ($_.ErrorDetails -and $_.ErrorDetails.Message) { $m = $m + ' | ' + $_.ErrorDetails.Message }
  # 交给 ConvertTo-Json 转义：手拼 JSON 遇到反斜杠 / 换行 / 引号就会拼出坏 JSON，
  # 客户端只能看到「传输异常」，真正的原因丢了
  [IO.File]::WriteAllText($Out, (@{ ok = $false; error = $m } | ConvertTo-Json -Compress -Depth 4), [Text.Encoding]::UTF8)
  'ERR ' + $m
}
