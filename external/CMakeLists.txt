cmake_minimum_required(VERSION 3.15)
project(fujimmich_external C)

include(FetchContent)

# libpict's option must be set before FetchContent_MakeAvailable runs its CMakeLists.
set(PTP_DEFAULT_LOGGING OFF CACHE BOOL "" FORCE)

FetchContent_Declare(
    libpict
    GIT_REPOSITORY https://github.com/petabyt/camlib.git
    GIT_TAG        86644ec76abf056a3d15fdeade576be3f5b4cee9
)
FetchContent_Declare(
    fp
    GIT_REPOSITORY https://github.com/petabyt/fp.git
    GIT_TAG        562df0d2de4ced7e3f0be587ed7d880a6dc14e3b
)
FetchContent_Declare(
    libfuji
    GIT_REPOSITORY https://github.com/petabyt/libfuji.git
    GIT_TAG        b5b730ee3066e0af37e9cd51335b67061d3cc838
)

# Declare libpict + fp first so libfuji's `if(NOT TARGET ...)` guards see them
# and don't try to re-add the subdirectories.
FetchContent_MakeAvailable(libpict fp libfuji)
