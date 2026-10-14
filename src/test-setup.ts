import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";

// happy-dom has no layout, so scrolling is a no-op.
Element.prototype.scrollIntoView = () => {};
window.scrollTo = () => {};

afterEach(cleanup);
