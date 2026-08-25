# 10: Reconciliación de borrados

**What to build:** que el calendario no acumule tareas fantasma que el profesor ya eliminó.

Sin esto, el calendario solo crece: cada assignment que Canvas deja de publicar queda para siempre. Con el tiempo la señal se pierde entre residuos.

Canvas es la fuente de verdad: lo que no está en Canvas hoy, no está en el calendario. Como `caldir push` propaga los borrados al servidor, alcanza con que el archivo local desaparezca.

Depende del ticket 06 y no del 05 porque necesita el seguimiento de estado que aquel introduce: para saber qué borrar hay que saber primero qué se escribió antes.

**Blocked by:** 06.

**Status:** ready-for-agent

- [ ] Un assignment presente en el estado previo y ausente de la respuesta de Canvas produce el borrado de sus componentes
- [ ] El borrado alcanza tanto al `VTODO` como al `VEVENT` del mismo assignment, cuando ambos existen
- [ ] El estado persistido deja de referenciar el assignment eliminado
- [ ] En modo simulación se informa qué se borraría y no se borra nada
- [ ] Un ramo que falla al consultarse **no** dispara el borrado de sus componentes: una respuesta ausente por error no es lo mismo que un assignment eliminado
- [ ] Los tests cubren: assignment eliminado, assignment presente, y fallo de consulta del ramo
