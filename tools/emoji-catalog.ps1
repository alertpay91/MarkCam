# Build crates\ovc-app\assets\emoji-catalog.tsv, the Effect Maker's emoji search list:
#   codepoint <TAB> name <TAB> keywords <TAB> flags
# flags: "a" = an animated version exists (Noto Animated Emoji, CC BY 4.0),
#        "3" = a 3D version exists (Fluent Emoji 3D, MIT; via the @lobehub/fluent-emoji-3d package).
# Codepoints are lowercase hex joined by "-" (without fe0f), as in the 3D package's file names.
#   powershell -ExecutionPolicy Bypass -File tools\emoji-catalog.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$tmp = Join-Path $env:TEMP 'markcam-emoji'
$null = New-Item -ItemType Directory -Force $tmp
Invoke-WebRequest 'https://unicode.org/Public/emoji/latest/emoji-test.txt' -OutFile "$tmp\emoji-test.txt" -UseBasicParsing
Invoke-WebRequest 'https://googlefonts.github.io/noto-emoji-animation/data/api.json' -OutFile "$tmp\noto.json" -UseBasicParsing
$list = Invoke-RestMethod 'https://data.jsdelivr.com/v1/packages/npm/@lobehub/fluent-emoji-3d@1.1.0?structure=flat' -UseBasicParsing

$three = @{}
foreach ($f in $list.files) {
    if ($f.name -match '^/assets/([0-9a-f-]+)\.webp$') { $three[$Matches[1].Replace('-fe0f', '')] = $Matches[1] }
}
$anim = @{}
foreach ($i in (Get-Content "$tmp\noto.json" -Raw | ConvertFrom-Json).icons) {
    $anim[$i.codepoint.Replace('_', '-').Replace('-fe0f', '')] = @{ id = $i.codepoint; tags = ($i.tags -join ' ').Replace(':', '').Replace('-', ' '); cat = ($i.categories -join ' ') }
}
$rows = New-Object Collections.Generic.List[string]
$group = ''
foreach ($line in Get-Content "$tmp\emoji-test.txt" -Encoding UTF8) {
    if ($line -match '^# subgroup: (.+)$') { $group = $Matches[1].Replace('-', ' '); continue }
    if ($line -notmatch '^([0-9A-F ]+);\s+fully-qualified\s+#\s+\S+\s+E[\d.]+\s+(.+)$') { continue }
    $cps = $Matches[1].Trim().ToLower() -split '\s+'
    $name = $Matches[2].Trim()
    # Skin tone variants: the default (yellow) one is enough.
    if ($cps | Where-Object { $_ -match '^1f3f[b-f]$' }) { continue }
    $key = (($cps | Where-Object { $_ -ne 'fe0f' }) -join '-')
    $flags = ''
    $kw = $group
    $code = $key
    if ($anim.ContainsKey($key)) { $flags += 'a'; $kw += ' ' + $anim[$key].tags; $code = $anim[$key].id + '|' + $key }
    else { $code = '|' + $key }
    if ($three.ContainsKey($key)) { $flags += '3'; $code += '|' + $three[$key] } else { $code += '|' }
    if (-not $flags) { continue }
    $rows.Add("$code`t$name`t$kw`t$flags")
}
$out = Join-Path $root 'crates\ovc-app\assets\emoji-catalog.tsv'
$null = New-Item -ItemType Directory -Force (Split-Path $out)
[IO.File]::WriteAllLines($out, $rows, (New-Object Text.UTF8Encoding($false)))
"{0} emoji ({1} animated, {2} 3D)" -f $rows.Count, ($rows | Where-Object { $_.EndsWith('a') -or $_.EndsWith('a3') }).Count, ($rows | Where-Object { $_.EndsWith('3') }).Count
