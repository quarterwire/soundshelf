# Draws the app icon and writes assets/icon.ico (multi-size, for the exe and Start menu)
# and assets/icon-64.rgba (raw RGBA, for the window/taskbar icon at runtime).
param([ValidateSet('SoundShelf', 'ScrubNotes')][string]$Style = 'SoundShelf')
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$assets = Join-Path (Split-Path -Parent $PSScriptRoot) 'assets'
New-Item -ItemType Directory -Force $assets | Out-Null

function New-RoundRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $p.AddArc($x, $y, $d, $d, 180, 90); $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90); $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure(); $p
}

function Draw-Icon([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'; $g.PixelOffsetMode = 'HighQuality'; $g.Clear([System.Drawing.Color]::Transparent)
    $s = $size / 256.0
    $g.ScaleTransform($s, $s)
    $m = 12; $box = 256 - 2 * $m
    if ($Style -eq 'SoundShelf') {
        # Dark tile with blue waveform bars.
        $bg = New-Object System.Drawing.Drawing2D.LinearGradientBrush((New-Object System.Drawing.PointF(0, 0)), (New-Object System.Drawing.PointF(0, 256)),
            [System.Drawing.Color]::FromArgb(255, 38, 42, 52), [System.Drawing.Color]::FromArgb(255, 20, 21, 25))
        $g.FillPath($bg, (New-RoundRect $m $m $box $box 52))
        $bar = New-Object System.Drawing.Drawing2D.LinearGradientBrush((New-Object System.Drawing.PointF(0, 50)), (New-Object System.Drawing.PointF(0, 206)),
            [System.Drawing.Color]::FromArgb(255, 130, 185, 255), [System.Drawing.Color]::FromArgb(255, 60, 120, 235))
        $heights = 46, 96, 150, 116, 70, 132, 160, 88, 52
        $bw = 15; $gap = 6.5; $x0 = 128 - ($heights.Count * $bw + ($heights.Count - 1) * $gap) / 2
        for ($i = 0; $i -lt $heights.Count; $i++) {
            $h = $heights[$i]; $x = $x0 + $i * ($bw + $gap)
            $g.FillPath($bar, (New-RoundRect $x (128 - $h / 2) $bw $h ($bw / 2)))
        }
    }
    else {
        # Blue tile with a white play button over a scrub timeline.
        $bg = New-Object System.Drawing.Drawing2D.LinearGradientBrush((New-Object System.Drawing.PointF(0, 0)), (New-Object System.Drawing.PointF(0, 256)),
            [System.Drawing.Color]::FromArgb(255, 72, 160, 245), [System.Drawing.Color]::FromArgb(255, 32, 110, 205))
        $g.FillPath($bg, (New-RoundRect $m $m $box $box 52))
        $white = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::White)
        $soft = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(110, 255, 255, 255))
        $tri = New-Object System.Drawing.Drawing2D.GraphicsPath
        $tri.AddPolygon([System.Drawing.PointF[]]@((New-Object System.Drawing.PointF(98, 56)), (New-Object System.Drawing.PointF(98, 152)), (New-Object System.Drawing.PointF(178, 104))))
        $g.FillPath($white, $tri)
        $g.FillPath($soft, (New-RoundRect 52 184 152 14 7))      # timeline
        $g.FillPath($white, (New-RoundRect 52 184 92 14 7))       # played part
        foreach ($mx in 82, 168) { $g.FillEllipse($white, $mx - 5, 166, 10, 10) }   # note markers
        $g.FillPath($white, (New-RoundRect 138 172 12 38 6))      # playhead
    }
    $g.Dispose()
    $bmp
}

# Standard ICO layout: 256 px as PNG, smaller sizes as 32-bit DIBs (readable by everything).
function To-Dib([System.Drawing.Bitmap]$b) {
    $n = $b.Width; $ms = New-Object IO.MemoryStream; $w = New-Object IO.BinaryWriter($ms)
    $maskStride = [int]([math]::Ceiling($n / 32.0) * 4)
    $w.Write([uint32]40); $w.Write([int32]$n); $w.Write([int32]($n * 2)); $w.Write([uint16]1); $w.Write([uint16]32)
    $w.Write([uint32]0); $w.Write([uint32]($n * $n * 4 + $maskStride * $n)); $w.Write([int32]0); $w.Write([int32]0); $w.Write([uint32]0); $w.Write([uint32]0)
    for ($y = $n - 1; $y -ge 0; $y--) { for ($x = 0; $x -lt $n; $x++) { $c = $b.GetPixel($x, $y); $w.Write([byte]$c.B); $w.Write([byte]$c.G); $w.Write([byte]$c.R); $w.Write([byte]$c.A) } }
    $w.Write((New-Object byte[] ($maskStride * $n)))   # AND mask unused with 32-bit alpha
    $w.Flush()
    return , $ms.ToArray()   # leading comma: return the byte[] itself, not its items one by one
}
$sizes = 256, 128, 64, 48, 32, 24, 16
$pngs = foreach ($sz in $sizes) {
    $b = Draw-Icon $sz
    if ($sz -ge 256) { $ms = New-Object IO.MemoryStream; $b.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png); [byte[]]$bytes = $ms.ToArray() } else { [byte[]]$bytes = To-Dib $b }
    $b.Dispose(); , $bytes
}
$out = New-Object IO.MemoryStream; $w = New-Object IO.BinaryWriter($out)
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $sz = $sizes[$i]; $dim = if ($sz -ge 256) { 0 } else { $sz }
    $w.Write([byte]$dim); $w.Write([byte]$dim); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32); $w.Write([uint32]$pngs[$i].Length); $w.Write([uint32]$offset)
    $offset += $pngs[$i].Length
}
foreach ($p in $pngs) { $w.Write([byte[]]$p) }
[IO.File]::WriteAllBytes((Join-Path $assets 'icon.ico'), $out.ToArray())

# Raw straight-alpha RGBA for egui::IconData.
$b = Draw-Icon 64
$rgba = New-Object byte[] (64 * 64 * 4)
for ($y = 0; $y -lt 64; $y++) { for ($x = 0; $x -lt 64; $x++) {
    $c = $b.GetPixel($x, $y); $o = ($y * 64 + $x) * 4
    $rgba[$o] = $c.R; $rgba[$o + 1] = $c.G; $rgba[$o + 2] = $c.B; $rgba[$o + 3] = $c.A
} }
[IO.File]::WriteAllBytes((Join-Path $assets 'icon-64.rgba'), $rgba)
$b.Dispose()
$b = Draw-Icon 256; $b.Save((Join-Path $assets 'icon.png'), [System.Drawing.Imaging.ImageFormat]::Png); $b.Dispose()
"wrote $assets"
