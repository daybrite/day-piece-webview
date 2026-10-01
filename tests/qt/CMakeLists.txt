cmake_minimum_required(VERSION 3.20)
project(day_webview_qt_tests LANGUAGES CXX)
enable_testing()
# The Windows QWidget host (src/lib-qt-windows.inc) compiled against a fake WebView2 bridge, so
# its startup scheduling and lifetime rules run on the Linux Qt job (docs/testing.md). QtWidgets
# is all it needs: no WebEngine, no COM.
find_package(Qt6 REQUIRED COMPONENTS Widgets)
add_executable(qt_startup startup.cpp)
target_compile_features(qt_startup PRIVATE cxx_std_17)
target_link_libraries(qt_startup PRIVATE Qt6::Widgets)
add_test(NAME qt_startup COMMAND qt_startup)
set_tests_properties(qt_startup PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen")
