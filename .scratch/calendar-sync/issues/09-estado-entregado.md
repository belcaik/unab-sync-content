# 09: Estado entregado o calificado

**What to build:** que lo ya entregado aparezca como hecho, sin desaparecer.

Incluye el caso que motivó esta decisión: un entregable grupal que entregó un compañero. Canvas propaga esa entrega al registro propio, así que consultarlo cubre el caso **sin lógica especial** para trabajos en grupo.

Lo entregado se marca completado en vez de borrarse, para conservar el registro de lo hecho.

Requiere un endpoint nuevo: las entregas propias del ramo, en una sola petición por curso en vez de una por assignment.

**Blocked by:** 05.

**Status:** ready-for-agent

- [ ] El flow consulta las entregas propias de cada ramo en una sola petición por curso
- [ ] Un assignment con fecha de entrega registrada se marca como completado
- [ ] Un assignment calificado se marca como completado, aunque no registre fecha de entrega
- [ ] Un assignment sin entrega ni calificación permanece pendiente
- [ ] El componente completado sigue existiendo en el calendario
- [ ] Los tests cubren los tres estados
- [ ] La petición pasa por el contexto HTTP y el paginador compartidos
- [ ] El endpoint queda agregado a la tabla de contrato de la API de Canvas en `AGENTS.md`, en el mismo commit
- [ ] Verificado a mano con un entregable grupal ya entregado por otra persona
