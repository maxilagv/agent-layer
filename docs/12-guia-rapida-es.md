# 12 · Guía rápida en español

LayerAgent es un agente de ingeniería local y autohospedado. El motor está en desarrollo; las interfaces de escritorio/web y el servicio SaaS no forman parte de esta versión.

## Instalar desde el código

Con Git, Rust 1.99, compilador C y Python 3.11+:

```bash
git clone https://github.com/maxilagv/agent-layer.git layeragent
cd layeragent
cargo build --release --locked -p layeragent-cli
./target/release/layeragent doctor
./target/release/layeragent init --protocol ollama --base-url http://127.0.0.1:11434 --model TU_MODELO_INSTALADO --context-window 32768
./target/release/layeragent run "Revisá el proyecto y sus pruebas" --workspace /ruta/al/proyecto
./target/release/layeragent tui --workspace /ruta/al/proyecto
```

Para ejecutar comandos y el navegador necesitás Linux con namespaces Bubblewrap y Landlock compatibles. Si falla el aislamiento, la ejecución se rechaza. macOS y Windows todavía no están habilitados como plataformas de ejecución certificadas.

## Configuración y seguridad

El estado se guarda fuera del workspace. Referenciá credenciales mediante variables de entorno o keyring; no las pegues en prompts o archivos del proyecto. Usá permisos y dominios acotados. Las acciones externas requieren aprobación.

El modo `privacy.local_models_only = true` restringe los modelos a endpoints locales. Para un entorno completamente sin conexiones externas también tenés que desactivar/restringir navegador, MCP, canales y red de herramientas.

## Tu perfil, primero el plan y el presupuesto

Contale al agente quién sos; lo usa para decidir qué construir y cómo explicarlo:

```bash
layeragent profile --technical none --language es --budget-usd 5 --publish "hosting estático"
layeragent profile            # muestra el perfil
```

`--technical` acepta `none` (no técnico: prefiere lo más simple que puedas abrir o publicar, como una página HTML), `some` (seguís instrucciones y hacés cambios chicos) o `expert` (desarrollador). `--budget-usd` es el presupuesto de las tareas que no fijan otro, y solo se aplica si el modelo tiene precios configurados.

Para que el agente planifique antes de construir:

```bash
layeragent run "Una página cinematográfica para vender bicicletas" --workspace . --plan
```

En modo plan investiga, escribe `docs/plan/` (`brief.md`, `stack.md`, `plan.md`, `acceptance.md`, `costs.md`) y te pide aprobación. No ejecuta comandos ni escribe fuera de `docs/plan/`. En la terminal interactiva usá `/plan <qué construir>`; cuando estés de acuerdo, `/build [notas]` construye lo aprobado.

Para controlar el gasto, `--budget-usd 3` (o `/budget 3` en la terminal) fija lo máximo que puede gastar la tarea, contando sus subagentes. El agente recibe avisos al 50, 80 y 95 % y puede consultar `budget_status`. `layeragent cost TAREA` muestra lo gastado por modelo, cuánto vino de la caché y sugerencias para gastar menos. Los costos son estimados con los precios que configuraste.

## Documentación

- [README principal](../README.md): instalación, capacidades y comandos.
- [Estado real y pendientes](10-implementation-status.md): qué está validado y qué falta.
- [Configuración y operación](11-configuration-and-operations.md).
- [Backups cifrados y restauración](13-backup-and-recovery.md).
- [Política de seguridad](../SECURITY.md).

Preparar un paquete local no publica el proyecto ni crea un repositorio Git.

## Permisos MCP y puente remoto

La integración MCP inicial es `project-files`: permisos por proyecto, herramienta, operación y destino, con restricciones reales en el broker Linux. Los servidores arbitrarios permanecen deshabilitados hasta contar con un adaptador verificable.

`layeragent remote --help` reúne emparejamiento, permisos, trabajos cifrados, estado, TUI, cancelación, desconexión y revocación. El coordinador usa PostgreSQL; cada equipo mantiene su autorización local y sus comprobantes de efectos. Las escrituras inciertas requieren revisión explícita, no reintentos automáticos.

Consultá la [instalación y guía operativa del puente remoto](14-remote-execution-and-mcp-trust.md) antes de habilitarlo.
