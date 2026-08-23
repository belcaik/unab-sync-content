# 03: Datos — fechas y metadata de assignment

**What to build:** que la información temporal y de calificación que Canvas ya expone deje de descartarse al deserializar.

Hoy un assignment se lee quedándose solo con identificador, nombre, descripción y fecha de modificación. Las fechas de entrega y disponibilidad, el puntaje y las banderas de calificación llegan en la misma respuesta HTTP y se tiran. Sin ellas no hay flow de calendario posible.

Las fechas pasan a ser **fechas tipadas**, no cadenas. El proyecto ya arrastra el costo de tratar timestamps como texto: la vista de estado ordena comparando cadenas y funciona solo por accidente del formato que emite Canvas. El flow nuevo no hereda eso.

**Blocked by:** None (can start immediately).

**Status:** ready-for-agent

- [ ] El tipo de assignment expone fecha de entrega, fecha de disponibilidad desde, fecha de cierre, puntaje posible, la bandera de exclusión de la nota final, la URL pública y los tipos de entrega
- [ ] Las fechas se deserializan como fechas, no como cadenas
- [ ] Un assignment sin ninguno de esos campos sigue deserializando sin error
- [ ] Un test cubre la deserialización a partir de una respuesta real de Canvas, incluyendo el caso de campos ausentes
- [ ] El flow de contenido, que consume el mismo tipo, sigue funcionando sin cambios
