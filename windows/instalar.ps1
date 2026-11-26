$ErrorActionPreference = "Stop"
$destino = "$env:LOCALAPPDATA\Programs\Onix"
$exe = "$PSScriptRoot\onix.exe", "$PSScriptRoot\..\instaladores\onix.exe" | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
if (-not $exe) {
    Write-Host "No se encontro onix.exe junto a este instalador." -ForegroundColor Yellow
    "1. Si descargaste el ZIP, extraelo completo (clic derecho, Extraer todo)."
    "2. Si aun falta, es posible que el antivirus lo haya quitado: revisa Seguridad de Windows,"
    "   Proteccion antivirus, Historial de proteccion. En un equipo de trabajo, pide ayuda a TI."
    exit 1
}
if (Get-Process onix -ErrorAction SilentlyContinue) {
    Write-Host "Onix esta abierto. Cierralo y vuelve a ejecutar el instalador." -ForegroundColor Yellow
    exit 1
}
New-Item -ItemType Directory -Force $destino | Out-Null
Copy-Item -LiteralPath $exe $destino -Force
$shell = New-Object -ComObject WScript.Shell
foreach ($carpeta in [Environment]::GetFolderPath("Programs"), [Environment]::GetFolderPath("Desktop")) {
    $acceso = $shell.CreateShortcut("$carpeta\Onix.lnk")
    $acceso.TargetPath = "$destino\onix.exe"
    $acceso.WorkingDirectory = $env:USERPROFILE
    $acceso.Save()
}
"Onix instalado en $destino, con acceso en el Escritorio y en el menu Inicio."

# Barras de contexto y uso en Claude Code. No pisa otra linea de estado que ya tengas.
try {
    Copy-Item "$PSScriptRoot\statusline.ps1" $destino -Force
    $claude = "$env:USERPROFILE\.claude"; $f = "$claude\settings.json"
    New-Item -ItemType Directory -Force $claude | Out-Null
    $j = $null
    if (Test-Path $f) { $j = Get-Content $f -Raw -Encoding UTF8 | ConvertFrom-Json }
    if (-not $j) { $j = [pscustomobject]@{} }
    if (-not $j.statusLine -or $j.statusLine.command -match "statusline\.(py|ps1)") {
        if (Test-Path $f) { Copy-Item $f "$f.bak" -Force }
        $ruta = "$destino\statusline.ps1".Replace('\', '/')
        $j | Add-Member statusLine @{ type = "command"; command = "powershell -NoProfile -ExecutionPolicy Bypass -File `"$ruta`"" } -Force
        [IO.File]::WriteAllText($f, ($j | ConvertTo-Json -Depth 100))
        "Barras de uso activadas en Claude Code."
    } else { "Ya tienes otra linea de estado: no se cambio." }
} catch { "No se pudieron activar las barras: $_" }
