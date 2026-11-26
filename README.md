# Ónix

## Mac (`mac/Instalar-Onix.dmg`)
Abre el archivo y arrastra Onix a Applications. Es para Mac con chip Apple. La app no está firmada con una cuenta
de Apple: en otro Mac, la primera vez hay que permitirla en Ajustes del Sistema, Privacidad y seguridad.

## Windows (`windows/instalar.cmd`)
Doble clic. Instala `onix.exe` para tu usuario y crea el acceso directo. Windows puede avisar que el programa
es desconocido porque no está firmado.

La versión de Windows usa el mismo código que la de Mac, con PowerShell. Aún está en pruebas.

| Atajo | Acción |
|---|---|
| Ctrl+Shift+T / W | Pestaña nueva / cerrar panel |
| Ctrl+Shift+D | Dividir en dos paneles |
| Ctrl+Shift+[ o ] | Cambiar de panel |
| Ctrl+Tab, Ctrl+1 a 9 | Cambiar de pestaña |
| Ctrl+Shift+B | Barra lateral |
| Ctrl+Shift+O | Elegir carpeta |
| Ctrl+Shift+G / P | Agentes / procesos (se apilan con puertos) |
| Ctrl+Shift+U | Agent usage (Esc cierra) |
| Ctrl+Shift+A, luego Ctrl+C | Seleccionar lo escrito y copiar |
| Ctrl+V / Ctrl+Shift+Z | Pegar / deshacer en Claude Code |
| Ctrl+Shift+K | Paleta de comandos |
| Ctrl+Shift+J | Ir al agente que te necesita |
| Ctrl+Shift+L | Puertos abiertos (Ctrl+⌫ detiene, Ctrl+C copia la dirección) |
| Ctrl+Shift+F | Buscar en el historial |
| Ctrl con +, − o 0 | Agrandar, achicar o restablecer la letra |

La barra lateral marca cada agente: azul trabajando, ámbar si te necesita y verde si terminó.

La vista previa flotante solo muestra imágenes.

## Código
- Mac: `mac/codigo` (repositorio Git).
- Windows: `windows/codigo`.
