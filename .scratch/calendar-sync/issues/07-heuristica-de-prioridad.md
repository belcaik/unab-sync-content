# 07: Heurística de prioridad

**What to build:** que las tareas que afectan la nota final se distingan a simple vista de las que no.

Un quiz sin peso y un entregable que vale el 30% se ven idénticos en una lista de vencimientos. La prioridad los separa.

**Mapeo decidido.** La RFC 5545 define que los clientes colapsan la escala 1–9 en tres buckets (1–4 alta, 5 media, 6–9 baja), así que se usan solo los tres valores representativos; los intermedios serían precisión que ningún cliente muestra.

| Valor | Condición |
|---|---|
| `PRIORITY:1` | Puntaje posible mayor a cero **y** no excluido de la nota final |
| `PRIORITY:5` | Admite entrega, pero no pesa en la nota final |
| `PRIORITY:9` | No admite entrega: lectura, informativo, o marcado como no calificable |

`PRIORITY:1` es la prioridad **más alta** de la escala. El valor `0` significa *sin definir* y no se emite en ningún caso.

La prioridad depende únicamente del estado en Canvas, nunca de cuán cerca está el vencimiento. Una prioridad que cambia sola cada día obligaría a reescribir componentes sin que Canvas haya cambiado, contradiciendo el ticket 06.

**Blocked by:** 05.

**Status:** ready-for-agent

- [ ] El planner asigna prioridad según la tabla anterior
- [ ] La prioridad se calcula sin consultar el instante actual
- [ ] Los tests cubren cada fila de la tabla
- [ ] Un test cubre el caso de puntaje mayor a cero pero excluido de la nota final, que **no** es prioridad alta
- [ ] Verificado a mano: el cliente de calendario muestra las tareas de prioridad alta diferenciadas
