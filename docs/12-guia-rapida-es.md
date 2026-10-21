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

## Documentación

- [README principal](../README.md): instalación, capacidades y comandos.
- [Estado real y pendientes](10-implementation-status.md): qué está validado y qué falta.
- [Configuración y operación](11-configuration-and-operations.md).
- [Backups cifrados y restauración](13-backup-and-recovery.md).
- [Política de seguridad](../SECURITY.md).

Preparar un paquete local no publica el proyecto ni crea un repositorio Git.
