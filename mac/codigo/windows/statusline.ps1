# Linea de estado de Claude Code: contexto, uso de 5 h y semanal. Solo ASCII: los simbolos van por codigo.
[Console]::InputEncoding = [Console]::OutputEncoding = New-Object Text.UTF8Encoding $false
$d = $null; try { $d = [Console]::In.ReadToEnd() | ConvertFrom-Json } catch {}
$e = [char]27; $gris = "$e[38;2;138;138;148m"; $fin = "$e[0m"

function Pct($v) { [int][Math]::Max(0, [Math]::Min(100, [Math]::Ceiling([double]$v - 1e-9))) }
function Falta($r) {
    if ("$r" -match '^[\d.]+$') { $t = [DateTimeOffset]::FromUnixTimeSeconds([long][double]$r) } else { $t = [DateTimeOffset]$r }
    $m = [long][Math]::Max(0, [Math]::Floor(($t - [DateTimeOffset]::Now).TotalMinutes))
    $dias = [Math]::Floor($m / 1440); $h = [Math]::Floor(($m % 1440) / 60); $mi = $m % 60
    if ($dias) { "${dias}d" } elseif ($h) { "${h}h ${mi}m" } else { "${mi}m" }
}
function Seg($nombre, $w, $largo, $reinicio) {
    if ($null -eq $w.used_percentage) { return "$gris$nombre$fin $([char]0x2014)" }
    $p = Pct $w.used_percentage; $n = [int][Math]::Ceiling($p * $largo / 100)
    $c = if ($p -ge 80) { 31 } elseif ($p -ge 60) { 33 } else { 39 }
    $s = "$gris$nombre$fin $e[${c}m$(([string][char]0x2501) * $n)$e[38;2;58;58;66m$(([string][char]0x2500) * ($largo - $n))$fin $p%"
    if ($reinicio -and $w.resets_at) { $s += " $gris$([char]0x21BB) $(Falta $w.resets_at)$fin" }
    $s
}

$ancho = 999; if ($env:COLUMNS) { $ancho = [int]$env:COLUMNS - 4 }
# Se usa la version mas larga que entre en la ventana.
foreach ($f in @(@(10, "semana", $true), @(5, "semana", $true), @(3, "sem", $true), @(3, "sem", $false))) {
    $partes = @()
    if ($null -ne $d.context_window.used_percentage) { $partes += "ctx $(Pct $d.context_window.used_percentage)%" }
    $partes += Seg "5h" $d.rate_limits.five_hour $f[0] $f[2]
    $partes += Seg $f[1] $d.rate_limits.seven_day $f[0] $f[2]
    $linea = $partes -join "   "
    if (($linea -replace "$e\[[0-9;]*m", "").Length -le $ancho) { break }
}
$linea
