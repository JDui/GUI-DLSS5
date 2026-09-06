cmake_minimum_required(VERSION 3.16)
project(rife_host CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

set(ROOT "${CMAKE_CURRENT_SOURCE_DIR}/..")
# era ncnn（2022-07，与 rife-v4.6 模型同期），由 tools/rife_upstream 的 CMake 工程从源码构建
set(NCNN_SRC "${ROOT}/tools/rife_upstream/src/ncnn" CACHE PATH "era ncnn source")
set(NCNN_BUILD "${ROOT}/tools/rife_upstream/build" CACHE PATH "era ncnn build tree")
set(VULKAN_INC "${ROOT}/tools/vulkan/include" CACHE PATH "Vulkan headers")
set(VULKAN_LIB "${ROOT}/tools/vulkan/lib/vulkan-1.lib" CACHE FILEPATH "vulkan-1 import library")

# Embed the compute shaders as byte arrays, mirroring the upstream
# generate_shader_comp_header.cmake (rife.cpp includes <name>.comp.hex.h).
set(SHADERS
    rife_preproc
    rife_postproc
    rife_preproc_tta
    rife_postproc_tta
    rife_flow_tta_avg
    rife_v2_flow_tta_avg
    rife_v4_flow_tta_avg
    rife_flow_tta_temporal_avg
    rife_v2_flow_tta_temporal_avg
    rife_v4_flow_tta_temporal_avg
    rife_out_tta_temporal_avg
    rife_v4_timestep
    rife_v4_timestep_tta
    warp
    warp_pack4
    warp_pack8)

set(GEN_DIR "${CMAKE_CURRENT_BINARY_DIR}/generated")
file(MAKE_DIRECTORY "${GEN_DIR}")

foreach(NAME ${SHADERS})
  file(READ "${CMAKE_CURRENT_SOURCE_DIR}/src/${NAME}.comp" comp_data)
  string(FIND "${comp_data}" "#version" version_start)
  string(SUBSTRING "${comp_data}" ${version_start} -1 comp_data)
  string(REGEX REPLACE "\n +" "\n" comp_data "${comp_data}")
  file(WRITE "${GEN_DIR}/${NAME}.hex.txt" "${comp_data}")
  file(READ "${GEN_DIR}/${NAME}.hex.txt" comp_hex HEX)
  string(REGEX REPLACE "([0-9a-f][0-9a-f])" "0x\\1," comp_hex "${comp_hex}")
  string(FIND "${comp_hex}" "," tail_comma REVERSE)
  string(SUBSTRING "${comp_hex}" 0 ${tail_comma} comp_hex)
  file(WRITE "${GEN_DIR}/${NAME}.comp.hex.h"
    "static const char ${NAME}_comp_data[] = {${comp_hex}};\n")
endforeach()

add_library(rife_host SHARED
  rife_host.cpp
  src/rife.cpp
  src/warp.cpp)

target_include_directories(rife_host PRIVATE
  "${CMAKE_CURRENT_SOURCE_DIR}/src"
  "${GEN_DIR}"
  "${NCNN_SRC}/include/ncnn"
  "${NCNN_SRC}/src"
  "${NCNN_BUILD}/ncnn/src"
  "${VULKAN_INC}")

target_link_libraries(rife_host PRIVATE
  "${NCNN_BUILD}/ncnn/src/Release/ncnn.lib"
  "${NCNN_BUILD}/ncnn/glslang/glslang/Release/glslang.lib"
  "${NCNN_BUILD}/ncnn/glslang/glslang/Release/MachineIndependent.lib"
  "${NCNN_BUILD}/ncnn/glslang/glslang/Release/GenericCodeGen.lib"
  "${NCNN_BUILD}/ncnn/glslang/glslang/OSDependent/Windows/Release/OSDependent.lib"
  "${NCNN_BUILD}/ncnn/glslang/OGLCompilersDLL/Release/OGLCompiler.lib"
  "${NCNN_BUILD}/ncnn/glslang/SPIRV/Release/SPIRV.lib"
  "${VULKAN_LIB}")

if(MSVC)
  # era ncnn 以 /MT 构建，宿主必须匹配静态 CRT
  set_property(TARGET rife_host PROPERTY MSVC_RUNTIME_LIBRARY "MultiThreaded")
  target_compile_options(rife_host PRIVATE /openmp /MP)
  target_compile_definitions(rife_host PRIVATE NOMINMAX)
endif()
