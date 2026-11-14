# Regenerates docs/themes/<id>.gif and <id>.png for every built-in theme.
#
# Needs Microsoft Edge (ships with Windows) and ffmpeg on PATH.
#   powershell -File docs/make-previews.ps1
#
# How it works: an ignored test renders each theme's welcome screen as a tall
# HTML page of animation frames; headless Edge screenshots the page; ffmpeg
# cuts the strip back into frames and writes the GIF.

$repo = Split-Path $PSScriptRoot -Parent
$edge = 'C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe'
$frames = 20           # must match FRAMES in preview_welcome_as_html
$w = 848; $h = 480     # one frame: 110 x 30 cells at 14px Cascadia Mono
$out = Join-Path $repo 'docs\themes'
$tmp = Join-Path $repo 'target\preview'

Push-Location $repo
try {
    cargo test preview_welcome -- --ignored 2>$null | Out-Null
    New-Item -ItemType Directory -Force $out | Out-Null
    foreach ($page in Get-ChildItem $tmp -Filter *.html) {
        $id = $page.BaseName
        $strip = Join-Path $tmp "$id.png"
        $url = 'file:///' + ($page.FullName -replace '\\', '/')
        # Edge returns before the file is written, so wait for it to appear
        Remove-Item $strip -ErrorAction SilentlyContinue
        & $edge --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=1 `
            "--window-size=$w,$($h * $frames)" "--screenshot=$strip" $url 2>$null | Out-Null
        for ($i = 0; $i -lt 100 -and -not (Test-Path $strip); $i++) { Start-Sleep -Milliseconds 100 }
        Start-Sleep -Milliseconds 300

        ffmpeg -v error -y -i $strip -vf "crop=${w}:${h}:0:0" (Join-Path $out "$id.png")
        ffmpeg -v error -y -i $strip -filter_complex `
            "untile=1x$frames,setpts=N/7/TB,split[a][b];[a]palettegen=max_colors=96[p];[b][p]paletteuse=dither=none" `
            -r 7 (Join-Path $out "$id.gif")
        Write-Host "  $id"
    }
}
finally { Pop-Location }
