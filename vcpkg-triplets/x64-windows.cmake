set(VCPKG_TARGET_ARCHITECTURE x64)
set(VCPKG_CRT_LINKAGE dynamic)
set(VCPKG_LIBRARY_LINKAGE dynamic)

# Keep the native codec ABI on the Visual Studio 2022 toolset. Hosted Windows
# images may also install newer Visual Studio toolsets; allowing vcpkg to pick
# the newest one makes the portable DLL set depend on runner-image drift.
set(VCPKG_PLATFORM_TOOLSET v143)
