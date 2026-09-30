PHP_ARG_ENABLE([atropos_shm],
  [whether to enable Atropos Nyx shared-memory support],
  [AS_HELP_STRING([--enable-atropos-shm], [Enable Atropos Nyx shared-memory support])],
  [yes])

if test "$PHP_ATROPOS_SHM" != "no"; then
  PHP_NEW_EXTENSION([atropos_shm], [atropos_shm.c], [$ext_shared])
fi
