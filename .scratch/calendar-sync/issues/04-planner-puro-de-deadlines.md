# 04: Planner puro — deadlines a componentes VTODO

**What to build:** la decisión de qué archivos de calendario deben existir, tomada por una función sin acceso a red, disco ni reloj.

Este es **el seam del feature**. Recibe un curso, sus assignments, el instante actual inyectado y el estado previo; devuelve un plan: qué escribir, con qué contenido, y qué borrar. Todo lo interesante —qué componente se emite, con qué identificador, con qué fechas— vive acá y se testea sin infraestructura.

El proyecto no tiene servidor HTTP simulado y este trabajo no lo introduce. Esa ausencia es precisamente por qué la lógica se concentra en una función pura.

En este ticket el planner solo entiende deadlines. Prioridad, ventanas y estado de entrega llegan después, ensanchando esta misma función.

**Blocked by:** 01 (el formato del componente depende de lo que el spike confirme), 03.

**Status:** ready-for-agent

- [ ] Existe una función pura que, dados curso, assignments, instante actual y estado previo, devuelve el plan de escrituras y borrados
- [ ] La función no realiza I/O ni consulta el reloj del sistema
- [ ] Cada assignment con fecha de entrega produce un componente `VTODO` con identificador estable, fecha de vencimiento, título y URL
- [ ] Un assignment sin fecha de entrega no produce ningún componente
- [ ] El identificador del componente se deriva del identificador del assignment en Canvas, de forma que sobreviva a cambios de título y de fecha
- [ ] Las fechas se emiten en UTC con sufijo `Z`
- [ ] Los tests cubren: assignment con fecha, assignment sin fecha, y un conjunto vacío de assignments
- [ ] Los tests afirman sobre el plan devuelto, sin inspeccionar el funcionamiento interno de la función
