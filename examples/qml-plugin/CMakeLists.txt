cmake_minimum_required(VERSION 3.21)
project(cljrs-qml LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
find_package(Qt6 REQUIRED COMPONENTS Qml)
qt_standard_project_setup(REQUIRES 6.5)

# Plugin-only QML module: the build directory itself becomes the import
# path (it gains a Cljrs/ subdir holding qmldir + the plugin).
qt_add_qml_module(cljrs_qml
    URI Cljrs
    VERSION 1.0
    PLUGIN_TARGET cljrs_qml
    SOURCES cljrsruntime.h cljrsruntime.cpp
)

# Build the runtime first: cargo build --release -p cljrs-qt (workspace root).
set(CLJRS_RUNTIME_DIR ${CMAKE_CURRENT_SOURCE_DIR}/../../target/release)
target_link_libraries(cljrs_qml PRIVATE
    Qt6::Qml
    ${CLJRS_RUNTIME_DIR}/libcljrs_qt.so
)
# The runtime .so stays in the cargo target dir; rpath it so the plugin
# finds it wherever the host process starts.
target_link_options(cljrs_qml PRIVATE "-Wl,-rpath,${CLJRS_RUNTIME_DIR}")
