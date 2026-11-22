# SPDX-License-Identifier: GPL-3.0-or-later
# Video probe for the Windows test VMs (in the rdp user's session, via
# `winvm.sh session-run`): a 640x360 window repainting moving content at 30 fps, so
# Windows classifies it as video and sends it as AVC420 (scripts/hwdec-check.sh).
#   video-probe.ps1 [-Seconds N]
param([int]$Seconds = 40)
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$form = New-Object Windows.Forms.Form
$form.Text = 'Transom video probe'
$form.FormBorderStyle = 'None'
$form.StartPosition = 'Manual'
$form.Location = New-Object Drawing.Point 200, 150
$form.ClientSize = New-Object Drawing.Size 640, 360
$form.TopMost = $true
$box = New-Object Windows.Forms.PictureBox
$box.Dock = 'Fill'
$form.Controls.Add($box)
$bmp = New-Object Drawing.Bitmap 640, 360
$g = [Drawing.Graphics]::FromImage($bmp)
$script:t = 0
$timer = New-Object Windows.Forms.Timer
$timer.Interval = 33
$timer.Add_Tick({
    $script:t++
    $t = $script:t
    for ($i = 0; $i -lt 12; $i++) {
        $c = [Drawing.Color]::FromArgb((($t * 7 + $i * 40) % 256), (($t * 3 + $i * 90) % 256), (($t * 5 + $i * 20) % 256))
        $b = New-Object Drawing.SolidBrush $c
        $g.FillRectangle($b, ($i * 53 + $t * 4) % 640, 0, 53, 360)
        $b.Dispose()
    }
    $g.FillEllipse([Drawing.Brushes]::White, ($t * 9) % 600, 160 + 100 * [Math]::Sin($t / 10), 40, 40)
    $box.Image = $bmp
    $box.Refresh()
    if ($t -gt $Seconds * 30) { $form.Close() }
})
$timer.Start()
[Windows.Forms.Application]::Run($form)
