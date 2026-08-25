# 13: Docker y cron

**What to build:** que el calendario se mantenga solo, sin que nadie se acuerde de ejecutarlo.

Hoy el proyecto se distribuye como binario y se corre a mano. El destino es un contenedor en un homeserver que corre el flow una vez al día y escribe en un volumen que otros contenedores consumen.

Las credenciales se inyectan en tiempo de ejecución y **no quedan en la imagen**, de forma que la imagen y el compose puedan compartirse sin filtrar el token de Canvas.

Un detalle del arranque que importa acá: el programa carga la configuración **antes** de despachar cualquier comando, y si el archivo falta lo crea y termina con código de error. En un contenedor recién levantado eso significa que la primera corrida falla salvo que la configuración esté montada de antemano.

**Blocked by:** 12.

**Status:** ready-for-agent

- [ ] Existe una imagen que contiene el binario compilado sin navegador headless
- [ ] La configuración y las credenciales se inyectan en tiempo de ejecución y no forman parte de la imagen
- [ ] El árbol de caldir se monta como volumen y los archivos escritos quedan accesibles desde el host con permisos utilizables
- [ ] Un archivo de composición levanta el servicio con el volumen y el cron ya configurados
- [ ] El cron ejecuta el flow una vez al día
- [ ] Un contenedor recién levantado con la configuración montada corre el flow sin intervención manual
- [ ] Un fallo del flow queda visible en los logs del contenedor con un código de salida distinguible
- [ ] Documentado el arranque completo: qué montar, qué variables definir, y cómo verificar la primera corrida
- [ ] Verificado end to end: el contenedor corre, escribe en el volumen, y `caldir push` desde su contenedor publica los cambios en Radicale
