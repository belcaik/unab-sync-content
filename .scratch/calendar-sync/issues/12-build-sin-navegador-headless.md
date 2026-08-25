# 12: Build sin navegador headless

**What to build:** poder compilar el flow de calendario sin arrastrar el navegador headless.

El flow de calendario no abre ningún navegador: habla con la API de Canvas y escribe archivos. Pero el proyecto depende de un navegador controlable que llega como dependencia de git —el build necesita red y git para resolverla— y esa dependencia es la parte frágil de la matriz de compilación, especialmente contra el target estático.

Separarla detrás de una bandera de compilación permite una imagen chica y un build reproducible para el contenedor del cron.

**Esta es una asunción del spec, no una decisión verificada.** Puede resultar más invasivo de lo que aparenta si el acoplamiento con el flow de Zoom es más profundo que lo que sugiere la estructura de módulos. Si al explorarlo el costo resulta desproporcionado, reportarlo antes de forzar la separación: el ticket 13 puede convivir con una imagen más grande.

**Blocked by:** 05.

**Status:** ready-for-agent

- [ ] Existe una bandera de compilación que excluye el navegador headless y el flow de Zoom
- [ ] Con esa bandera, el proyecto compila sin resolver la dependencia de git
- [ ] Con esa bandera, el flow de calendario funciona completo
- [ ] Sin esa bandera, todo el comportamiento actual se conserva
- [ ] El comando de Zoom, cuando está excluido, informa con claridad que esa función no está disponible en este build
- [ ] La compilación sin la bandera queda verificada contra el target estático, antes de cualquier tag de release
- [ ] La matriz de integración continua cubre ambas configuraciones
- [ ] Las dos formas de compilar quedan documentadas
