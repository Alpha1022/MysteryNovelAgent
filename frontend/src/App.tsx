import { HashRouter, Routes, Route } from "react-router-dom";
import LibraryPage from "./pages/LibraryPage";
import BookDetailPage from "./pages/BookDetailPage";
import Chatbot from "./components/Chatbot";
import PathPickerHost from "./components/PathPickerHost";

export default function App() {
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
