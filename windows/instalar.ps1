$ErrorActionPreference = "Stop"
$destino = "$env:LOCALAPPDATA\Programs\Onix"
New-Item -ItemType Directory -Force $destino | Out-Null
Copy-Item "$PSScriptRoot\onix.exe" $destino -Force
$shell = New-Object -ComObject WScript.Shell
foreach ($carpeta in [Environment]::GetFolderPath("Programs"), [Environment]::GetFolderPath("Desktop")) {
    $acceso = $shell.CreateShortcut("$carpeta\Onix.lnk")
    $acceso.TargetPath = "$destino\onix.exe"
    $acceso.WorkingDirectory = $env:USERPROFILE
    $acceso.Save()
}
"Onix instalado en $destino, con acceso en el Escritorio y en el menu Inicio."
