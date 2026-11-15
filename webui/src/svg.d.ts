// SVG files are imported as text (angular.json: "loader": {".svg": "text"}).
declare module '*.svg' {
  const svg: string;
  export default svg;
}
