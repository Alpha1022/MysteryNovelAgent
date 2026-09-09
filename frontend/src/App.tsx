import { useEffect } from "react";
import { HashRouter, Routes, Route } from "react-router-dom";
import LibraryPage from "./pages/LibraryPage";
import BookDetailPage from "./pages/BookDetailPage";
import Chatbot from "./components/Chatbot";
import PathPickerHost from "./components/PathPickerHost";

/**
 * 全局拖放防导航：dragDropEnabled 关闭后 WebView2 放行外部拖拽（书架页据此实现
 * HTML5 拖拽导入），但未 preventDefault 的页面会把拖入的文件当成导航目标、
 * 整个应用被替换成文件内容。此处在应用层拦截文件类拖放；非文件拖拽
 * （页面内文本拖动等）不受影响。实际导入逻辑在 LibraryPage（仅书架响应）。
 */
function useBlockFileDropNavigation() {
  useEffect(() => {
    const hasFiles = (e: DragEvent) =>
      Array.from(e.dataTransfer?.types ?? []).includes("Files");
    const onOver = (e: DragEvent) => {
      if (hasFiles(e)) e.preventDefault();
    };
    const onDrop = (e: DragEvent) => {
      if (hasFiles(e)) e.preventDefault();
    };
    window.addEventListener("dragover", onOver);
    window.addEventListener("drop", onDrop);
    return () => {
      window.removeEventListener("dragover", onOver);
      window.removeEventListener("drop", onDrop);
    };
  }, []);
}

export default function App() {
  useBlockFileDropNavigation();
  return (
    <HashRouter>
      <Routes>
        <Route path="/" element={<LibraryPage />} />
        <Route path="/book/:id" element={<BookDetailPage />} />
      </Routes>
      <Chatbot />
      <PathPickerHost />
    </HashRouter>
  );
}
