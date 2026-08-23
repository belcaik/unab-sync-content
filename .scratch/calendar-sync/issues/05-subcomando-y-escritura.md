# 05: Subcomando y escritura — el flow completo end to end

**What to build:** que ejecutar un comando deje los vencimientos de todos los ramos activos visibles en el cliente de calendario.

Este es el ticket donde el feature existe por primera vez. Conecta lo que Canvas devuelve con el planner y con el disco: trae los assignments de los ramos activos, arma el plan, y lo escribe en el árbol de caldir de forma atómica.

Es el primer punto en el que algo es demoable: los tickets 03 y 04 preparan las piezas, este las hace funcionar juntas.

Respeta la lista de ramos ignorados que ya existe en la configuración, para reusar lo que el usuario ya configuró.

**Blocked by:** 02, 04.

**Status:** ready-for-agent

- [ ] Un subcomando nuevo, hermano de los existentes, ejecuta el flow de calendario
- [ ] El comando acepta filtrar por un ramo puntual y una bandera de simulación
- [ ] En modo simulación el comando informa qué haría y no escribe ni un byte
- [ ] Los ramos listados como ignorados en la configuración no generan calendarios
- [ ] Los archivos se escriben con escritura atómica, reusando el mecanismo ya presente en el proyecto
- [ ] Toda petición HTTP pasa por el contexto HTTP compartido; el listado de assignments usa el paginador compartido
- [ ] Las claves de configuración que se agreguen son leídas por el código y quedan reflejadas en la plantilla de configuración en el mismo commit
- [ ] La salida al usuario pasa por el mecanismo de estado del proyecto; el código de biblioteca emite trazas, no escribe a stdout
- [ ] Un error del flow devuelve el código de salida de runtime que usa el resto de los comandos
- [ ] Los endpoints usados quedan agregados a la tabla de contrato de la API de Canvas en `AGENTS.md`, en el mismo commit
- [ ] La estructura de directorios que produce queda documentada en el README
- [ ] Verificado a mano: tras ejecutar el comando y hacer `caldir push`, los vencimientos aparecen en el cliente de calendario
