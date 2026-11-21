# SPDX-License-Identifier: GPL-3.0-or-later
# Latency probe for the Windows test VMs: a borderless, maximized, topmost window
# that repaints entirely in another colour on every key press (the counterpart of
# the grd probe for crates/transom-view-freerdp/examples/latency.rs). Exits after
# $Seconds. Run in the rdp user's session (`winvm.sh session-run`).
param([int]$Seconds = 60)
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$form = New-Object Windows.Forms.Form
$form.FormBorderStyle = 'None'; $form.WindowState = 'Maximized'; $form.TopMost = $true
$form.BackColor = [Drawing.Color]::FromArgb(20, 40, 160)
$form.KeyPreview = $true
$colors = @([Drawing.Color]::FromArgb(20, 40, 160), [Drawing.Color]::FromArgb(230, 120, 30))
$script:i = 0
$form.Add_KeyDown({ $script:i = 1 - $script:i; $form.BackColor = $colors[$script:i]; $form.Refresh() })
$timer = New-Object Windows.Forms.Timer; $timer.Interval = $Seconds * 1000
$timer.Add_Tick({ $form.Close() }); $timer.Start()
$form.Add_Shown({ $form.Activate() })
[Windows.Forms.Application]::Run($form)
