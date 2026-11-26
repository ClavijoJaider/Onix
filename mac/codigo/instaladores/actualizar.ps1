# "onix update": baja el onix.exe publicado en GitHub y reemplaza el instalado.
$ErrorActionPreference = "Stop"
$url = "https://raw.githubusercontent.com/ClavijoJaider/Onix/main/windows/onix.exe"
$destino = "$env:LOCALAPPDATA\Programs\Onix"
$exe = "$destino\onix.exe"
$nuevo = "$destino\onix.new.exe"
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    $ProgressPreference = "SilentlyContinue"
    "Buscando la ultima version..."
    Invoke-WebRequest $url -OutFile $nuevo -UseBasicParsing
    $inicio = [IO.File]::ReadAllBytes($nuevo)
    if ($inicio.Length -lt 100000 -or $inicio[0] -ne 0x4D -or $inicio[1] -ne 0x5A) { throw "La descarga no es un programa valido." }
    if ((Test-Path $exe) -and (Get-FileHash $exe).Hash -eq (Get-FileHash $nuevo).Hash) {
        Remove-Item $nuevo
        "Onix ya esta al dia."
        exit 0
    }
    # Windows no deja sobrescribir un programa abierto, pero si cambiarle el nombre.
    Get-ChildItem $destino -Filter "onix.old*.exe" | Remove-Item -ErrorAction SilentlyContinue
    $viejo = "$destino\onix.old-$(Get-Date -Format yyyyMMddHHmmss).exe"
    if (Test-Path $exe) { Rename-Item $exe $viejo }
    try { Rename-Item $nuevo $exe }
    catch { if (Test-Path $viejo) { Rename-Item $viejo $exe }; throw }
    "Listo. Cierra y vuelve a abrir Onix."
} catch {
    Remove-Item $nuevo -ErrorAction SilentlyContinue
    Write-Host "No se pudo actualizar: $_" -ForegroundColor Yellow
    exit 1
}
