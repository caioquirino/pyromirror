#!/bin/sh
# Runs the real pyromirror-server or pyromirror-client, whichever this file is installed as.
#
# PyroWave needs Vulkan subgroups of 16 or more, which Mesa's software driver only offers with
# 512-bit vectors (it defaults to 256). Mesa's software OpenGL crashes with that setting, so it is
# kept to these two programs.
export LP_NATIVE_VECTOR_WIDTH=512
# shellcheck disable=SC2086
exec "/opt/pyromirror/libexec/$(basename "$0")" "$@" ${PYROMIRROR_ARGS:-}
