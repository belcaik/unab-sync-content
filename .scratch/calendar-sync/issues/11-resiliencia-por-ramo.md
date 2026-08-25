# 11: Resiliencia por ramo

**What to build:** que un ramo con problemas no borre el calendario de los demás.

Corriendo por cron y sin supervisión, un fallo puntual en un ramo dejaría sin actualizar la semana entera. El flow procesa cada ramo de forma independiente: los que responden bien se sincronizan, el que falla se informa, y la corrida termina reportando qué pasó.

El flow de contenido hace lo contrario —un fallo aborta la corrida completa— y eso ya está documentado como aspereza conocida. El flow de calendario **no reproduce ese comportamiento**. Este ticket no modifica el flow de contenido.

**Blocked by:** 05.

**Status:** ready-for-agent

- [ ] Un fallo al consultar un ramo permite que los demás se sincronicen normalmente
- [ ] El ramo que falla queda registrado en las trazas con su identificador
- [ ] Al terminar, el comando informa cuántos ramos se sincronizaron y cuántos fallaron
- [ ] Si al menos un ramo falla, el código de salida lo refleja, de forma que el cron pueda alertar
- [ ] Si todos los ramos fallan, el comando falla
- [ ] Verificado a mano apuntando a un identificador de ramo inexistente junto a ramos válidos
