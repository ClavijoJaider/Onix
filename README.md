# Ónix

Ónix es una terminal para macOS escrita en Rust, pensada para trabajar con agentes de código como Claude Code. Es liviana, tiene tema oscuro y no envía telemetría ni se conecta a la red por su cuenta. Los programas que ejecutes dentro sí pueden hacerlo.

## Qué trae

- Pestañas y hasta dos paneles lado a lado.
- Una barra lateral con tus sesiones agrupadas por proyecto, con su estado y un buscador.
- Reconoce Claude Code, Codex, Antigravity, Grok, opencode y Junie. Cada uno se instala aparte y usa tu propia cuenta.
- Al reabrir la app vuelven tus pestañas. Las pestañas con agente que cierres quedan en "Archived" para retomarlas.
- Un chat de Claude Code que lleve 15 minutos esperando se puede dormir para liberar memoria. Se retoma con Enter.
- El panel "Agent usage" muestra tokens, tareas, tiempo y horas de más actividad. Por ahora solo cuenta el uso de Claude Code.

En mediciones del 25 de noviembre de 2026, con la ventana a pantalla completa en un Mac Retina, usó entre 56 y 89 MB de memoria.

## Instalación

Necesitas macOS y [Rust](https://rustup.rs). La primera compilación descarga las dependencias.

```bash
git clone https://github.com/ClavijoJaider/Onix.git
cd Onix
scripts/build-app.sh
```

La app queda en `~/Applications/Onix.app`. El resultado de la compilación se guarda en `~/Library/Logs/mi-terminal-build.log`.

## Atajos

| Atajo | Acción |
|---|---|
| ⌘T | Pestaña nueva |
| ⇧⌘O | Elegir una carpeta |
| ⌘W | Cerrar el panel actual |
| ⌘1 a ⌘9 | Ir a una pestaña |
| ⌃Tab | Pestaña siguiente |
| ⌘D | Dividir en dos paneles |
| ⌘[ o ⌘] | Cambiar de panel |
| ⌘B | Mostrar u ocultar la barra lateral |
| ⇧⌘A | Lista de agentes |
| ⇧⌘P | Lista de procesos |
| ⇧⌘U | Panel "Agent usage" (se cierra con Esc) |
| ⌘C y ⌘V | Copiar y pegar |
| ⇧RePág y ⇧AvPág | Recorrer el historial |

## Tus datos

Ónix lee los archivos de `~/.claude` para mostrar las sesiones y las cifras de uso, sin modificarlos. Lo suyo lo guarda en `~/Library/Application Support/mi-terminal`.

## Línea de estado para Claude Code

La carpeta [`plugins`](plugins/README.md) trae una línea de estado opcional con el contexto consumido y los límites de uso. Si clonaste el proyecto en otra carpeta, ajusta la ruta que aparece en sus instrucciones.

## Desarrollo

```bash
cargo test
cargo run --release -p usage -- --days 30
```

El segundo comando imprime tu consumo de tokens de los últimos 30 días.

El código está dividido en tres partes:

- `crates/app`: la ventana, la interfaz y el dibujo en CPU.
- `crates/term-core`: la conexión con el shell y la emulación de terminal.
- `crates/usage`: la lectura y el conteo del uso.

## Logos

Los logos de los agentes pertenecen a sus dueños. Su origen está en [ARCHIVO-FUENTES.md](crates/app/assets/icons/ARCHIVO-FUENTES.md).
