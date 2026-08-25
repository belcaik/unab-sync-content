# 01: Spike — round-trip de VTODO por caldir a Radicale

**What to build:** certeza empírica de que un `VTODO` sobrevive el viaje completo `archivo → caldir push → Radicale → caldir pull → archivo` sin perder información.

Toda la decisión central del spec (deadline = `VTODO` tildeable) descansa sobre esto, y la documentación de caldir habla únicamente de *events*. Nadie afirma que VTODO falle; nadie afirma que funcione. Hasta que este ticket cierre, el resto del flow se está construyendo sobre una suposición.

El spike termina en una **respuesta escrita**, no en código de producción. Lo que se escriba acá se tira.

**Blocked by:** None (can start immediately).

**Status:** ready-for-agent

- [ ] Radicale y caldir corriendo localmente, con un calendario de prueba conectado por CalDAV
- [ ] Un `VTODO` escrito a mano en el árbol de caldir llega a Radicale con `UID`, `DUE`, `PRIORITY`, `STATUS` y `SUMMARY` intactos
- [ ] El mismo `VTODO` vuelve por `pull` sin que ninguno de esos campos se altere
- [ ] Documentado qué hace caldir cuando cambia la fecha de un componente: ¿renombra el archivo, deja el viejo, o reconcilia por el `UID` interno?
- [ ] Documentado qué nombre de archivo le da caldir a un `VTODO`, que no tiene `DTSTART` obligatorio
- [ ] Documentado si al borrar un archivo local, `push` propaga el borrado al servidor
- [ ] Los hallazgos quedan escritos en el spec, y si contradicen el modelo VTODO+VEVENT, el spec se corrige antes de que empiece cualquier ticket de implementación
